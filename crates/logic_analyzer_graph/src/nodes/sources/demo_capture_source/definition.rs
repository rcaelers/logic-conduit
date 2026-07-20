//! `Demo Capture Source` graph-node definition.

use egui::Color32;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use node_graph::{InputDef, NodeBadge, NodeDef, OutputDef};
use signal_processing::{SimpleTriggerCondition, TriggerProgram};

use crate::nodes::registry::{COLOR_SOURCES, Signal};

pub(crate) const DEMO_CAPTURE_CHANNELS: usize = 11;
const DEMO_CAPTURE_STATE_VERSION: u16 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DemoCaptureSourceState {
    schema_version: u16,
    trigger_program: Option<TriggerProgram>,
    #[serde(skip)]
    compatibility_warning: Option<String>,
}

impl Default for DemoCaptureSourceState {
    fn default() -> Self {
        Self {
            schema_version: DEMO_CAPTURE_STATE_VERSION,
            trigger_program: None,
            compatibility_warning: None,
        }
    }
}

impl DemoCaptureSourceState {
    pub fn trigger_program(&self) -> Option<&TriggerProgram> {
        self.trigger_program.as_ref()
    }

    pub fn set_trigger_program(&mut self, program: Option<TriggerProgram>) -> Result<(), String> {
        super::trigger::validate_program(program.as_ref())?;
        self.trigger_program = program;
        self.compatibility_warning = None;
        Ok(())
    }

    pub fn set_trigger_condition(
        &mut self,
        channel: usize,
        condition: SimpleTriggerCondition,
    ) -> Result<(), String> {
        self.trigger_program =
            super::trigger::set_condition(self.trigger_program.as_ref(), channel, condition)?;
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

        let trigger_program = if object.contains_key("trigger_program") {
            let parsed = match object.get("trigger_program") {
                Some(Value::Null) => Some(None),
                Some(value) => match serde_json::from_value::<TriggerProgram>(value.clone()) {
                    Ok(program) => Some(Some(program)),
                    Err(error) => {
                        warnings.push(format!(
                            "reset malformed trigger program to free run: {error}"
                        ));
                        None
                    }
                },
                None => unreachable!(),
            };
            match parsed {
                Some(program) if super::trigger::validate_program(program.as_ref()).is_ok() => {
                    program
                }
                Some(program) => {
                    let error = super::trigger::validate_program(program.as_ref())
                        .expect_err("program was already known to be incompatible");
                    warnings.push(format!(
                        "reset incompatible trigger program to free run: {error}"
                    ));
                    None
                }
                None => None,
            }
        } else {
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
            if saved_conditions.is_some_and(|conditions| conditions.len() != DEMO_CAPTURE_CHANNELS)
            {
                warnings.push(format!(
                    "normalized trigger input count to {DEMO_CAPTURE_CHANNELS}"
                ));
            }
            super::trigger::program_from_conditions(&trigger_conditions)
                .map_err(serde::de::Error::custom)?
        };

        warnings.sort();
        warnings.dedup();
        Ok(Self {
            schema_version: DEMO_CAPTURE_STATE_VERSION,
            trigger_program,
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

    use super::super::trigger;
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
            trigger::conditions(restored.trigger_program()).unwrap(),
            [Ignore; DEMO_CAPTURE_CHANNELS]
        );
        let warning = DemoCaptureSource::badge(&restored).unwrap();
        assert!(warning.text.contains("legacy"));

        let saved = serde_json::to_value(restored).unwrap();
        assert_eq!(saved["schema_version"], 2);
        assert!(saved.get("trigger_conditions").is_none());
        assert!(saved["trigger_program"].is_null());
    }

    #[test]
    fn malformed_trigger_entries_are_normalized_with_a_diagnostic() {
        let restored: DemoCaptureSourceState = serde_json::from_value(serde_json::json!({
            "schema_version": 0,
            "trigger_conditions": ["high", "future_condition"]
        }))
        .unwrap();
        let conditions = trigger::conditions(restored.trigger_program()).unwrap();
        assert_eq!(conditions[0], High);
        assert_eq!(conditions[1], Ignore);
        let warning = DemoCaptureSource::badge(&restored).unwrap();
        assert!(warning.text.contains("trigger input 1"));
        assert!(warning.text.contains("schema 0"));
    }

    #[test]
    fn incompatible_saved_program_resets_visibly_instead_of_disappearing_silently() {
        let mut state = DemoCaptureSourceState::default();
        state.set_trigger_condition(4, Falling).unwrap();
        let mut saved = serde_json::to_value(state).unwrap();
        saved["trigger_program"]["schema_revision"] = serde_json::json!(99);

        let restored: DemoCaptureSourceState = serde_json::from_value(saved).unwrap();

        assert!(restored.trigger_program().is_none());
        let warning = DemoCaptureSource::badge(&restored).unwrap();
        assert!(warning.text.contains("reset incompatible trigger program"));
    }

    #[test]
    fn malformed_saved_program_resets_with_a_visible_warning() {
        let mut saved = serde_json::to_value(DemoCaptureSourceState::default()).unwrap();
        saved["trigger_program"] = serde_json::json!({ "stages": "not-an-array" });

        let restored: DemoCaptureSourceState = serde_json::from_value(saved).unwrap();

        assert!(restored.trigger_program().is_none());
        let warning = DemoCaptureSource::badge(&restored).unwrap();
        assert!(warning.text.contains("reset malformed trigger program"));
    }
}
