//! `Demo Capture Source` graph-node definition.

use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use node_graph::{InputDef, NodeBadge, NodeDef, OutputDef};
use signal_processing::SimpleTriggerCondition;

use crate::nodes::registry::{COLOR_SOURCES, Signal};

pub const DEMO_CAPTURE_CHANNELS: usize = 11;
const DEMO_CAPTURE_STATE_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DemoCaptureSourceState {
    schema_version: u16,
    trigger_conditions: Vec<SimpleTriggerCondition>,
    #[serde(skip)]
    compatibility_warning: Option<String>,
}

impl Default for DemoCaptureSourceState {
    fn default() -> Self {
        Self {
            schema_version: DEMO_CAPTURE_STATE_VERSION,
            trigger_conditions: vec![SimpleTriggerCondition::Ignore; DEMO_CAPTURE_CHANNELS],
            compatibility_warning: None,
        }
    }
}

impl DemoCaptureSourceState {
    pub fn trigger_conditions(&self) -> &[SimpleTriggerCondition] {
        &self.trigger_conditions
    }

    pub fn set_trigger_condition(
        &mut self,
        channel: usize,
        condition: SimpleTriggerCondition,
    ) -> Result<(), String> {
        let Some(current) = self.trigger_conditions.get_mut(channel) else {
            return Err(format!(
                "demo capture channel {channel} is outside 0..{DEMO_CAPTURE_CHANNELS}"
            ));
        };
        *current = condition;
        self.compatibility_warning = None;
        Ok(())
    }
}

impl<'de> Deserialize<'de> for DemoCaptureSourceState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let Some(object) = value.as_object() else {
            return Err(serde::de::Error::custom(
                "demo capture state must be an object",
            ));
        };
        if object.is_empty() {
            return Ok(Self {
                compatibility_warning: Some(
                    "Updated legacy demo capture settings; trigger inputs defaulted to Ignore"
                        .to_owned(),
                ),
                ..Self::default()
            });
        }

        let saved_version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|version| u16::try_from(version).ok());
        let mut warnings = Vec::new();
        if saved_version != Some(DEMO_CAPTURE_STATE_VERSION) {
            warnings.push(format!(
                "updated demo capture settings from schema {} to {}",
                saved_version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                DEMO_CAPTURE_STATE_VERSION
            ));
        }

        let saved_conditions = object.get("trigger_conditions").and_then(Value::as_array);
        let mut trigger_conditions = Vec::with_capacity(DEMO_CAPTURE_CHANNELS);
        for channel in 0..DEMO_CAPTURE_CHANNELS {
            let condition = saved_conditions
                .and_then(|conditions| conditions.get(channel))
                .cloned()
                .and_then(|condition| {
                    serde_json::from_value::<SimpleTriggerCondition>(condition).ok()
                });
            match condition {
                Some(condition) => trigger_conditions.push(condition),
                None => {
                    trigger_conditions.push(SimpleTriggerCondition::Ignore);
                    warnings.push(format!("trigger input {channel} defaulted to Ignore"));
                }
            }
        }
        if saved_conditions.is_some_and(|conditions| conditions.len() != DEMO_CAPTURE_CHANNELS) {
            warnings.push(format!(
                "normalized trigger input count to {DEMO_CAPTURE_CHANNELS}"
            ));
        }

        warnings.sort();
        warnings.dedup();
        Ok(Self {
            schema_version: DEMO_CAPTURE_STATE_VERSION,
            trigger_conditions,
            compatibility_warning: (!warnings.is_empty()).then(|| warnings.join("; ")),
        })
    }
}

pub struct DemoCaptureSource;

impl NodeDef for DemoCaptureSource {
    type State = DemoCaptureSourceState;

    fn name() -> &'static str {
        "Demo Capture Source"
    }

    fn category() -> &'static str {
        "Sources"
    }

    fn color() -> Color32 {
        COLOR_SOURCES
    }

    fn inputs() -> Vec<InputDef<Self::State>> {
        vec![]
    }

    fn outputs() -> Vec<OutputDef<Self::State>> {
        (0..DEMO_CAPTURE_CHANNELS)
            .map(|channel| OutputDef::new::<Signal>(format!("Ch {channel}")))
            .collect()
    }

    fn state() -> Self::State {
        DemoCaptureSourceState::default()
    }

    fn badge(state: &Self::State) -> Option<NodeBadge> {
        state.compatibility_warning.as_ref().map(NodeBadge::warning)
    }
}

#[cfg(test)]
mod tests {
    use node_graph::NodeDef;
    use signal_processing::SimpleTriggerCondition::{Falling, High, Ignore};

    use super::{DEMO_CAPTURE_CHANNELS, DemoCaptureSource, DemoCaptureSourceState};

    #[test]
    fn current_state_round_trips_every_trigger_condition_without_a_warning() {
        let mut state = DemoCaptureSourceState::default();
        state.set_trigger_condition(2, High).unwrap();
        state.set_trigger_condition(9, Falling).unwrap();
        let saved = serde_json::to_value(&state).unwrap();
        let restored: DemoCaptureSourceState = serde_json::from_value(saved).unwrap();
        assert_eq!(restored, state);
        assert!(DemoCaptureSource::badge(&restored).is_none());
    }

    #[test]
    fn legacy_empty_state_migrates_explicitly_and_reports_a_warning() {
        let restored: DemoCaptureSourceState =
            serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(
            restored.trigger_conditions(),
            &[Ignore; DEMO_CAPTURE_CHANNELS]
        );
        let warning = DemoCaptureSource::badge(&restored).unwrap();
        assert!(warning.text.contains("legacy"));

        let saved = serde_json::to_value(restored).unwrap();
        assert_eq!(saved["schema_version"], 1);
        assert_eq!(
            saved["trigger_conditions"].as_array().unwrap().len(),
            DEMO_CAPTURE_CHANNELS
        );
    }

    #[test]
    fn malformed_trigger_entries_are_normalized_with_a_diagnostic() {
        let restored: DemoCaptureSourceState = serde_json::from_value(serde_json::json!({
            "schema_version": 0,
            "trigger_conditions": ["high", "future_condition"]
        }))
        .unwrap();
        assert_eq!(restored.trigger_conditions()[0], High);
        assert_eq!(restored.trigger_conditions()[1], Ignore);
        let warning = DemoCaptureSource::badge(&restored).unwrap();
        assert!(warning.text.contains("trigger input 1"));
        assert!(warning.text.contains("schema 0"));
    }
}
