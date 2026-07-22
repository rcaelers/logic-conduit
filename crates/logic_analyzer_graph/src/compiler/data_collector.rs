//! Presentation-neutral collection of retained derived data.

use std::collections::HashMap;

use serde_json::Value;

use node_graph::Socket;
use signal_processing::{
    CollectedLaneRequest, CollectedPayloadRegistry, CollectedWordLaneOptions, DerivedDataCollector,
    LiveStoreConfig, NumberSample, ProcessNode, Sample, TextSample, Trigger, Word,
};

use super::{CompileCtx, PortKind, ResolvedInputs, RuntimeBuilder};

pub(crate) const BUILDER_NAME: &str = "Derived Data Collector";

pub(crate) struct DataCollectorBuilder;

impl DataCollectorBuilder {
    pub(crate) fn build_with_lane_names(
        name: &str,
        resolved: &ResolvedInputs,
        lane_names: &[(usize, String)],
        collected_payloads: &CollectedPayloadRegistry,
        ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let mut collector = DerivedDataCollector::new(ctx.derived_lanes().clone())
            .with_name(name)
            .with_retention(ctx.derived_data_retention());
        for (member, lane_name) in lane_names {
            let input = resolved
                .get(0, *member)
                .ok_or_else(|| format!("collector input {member} is unresolved"))?;
            let descriptor = collected_payloads
                .descriptor_by_type_id(input.kind.type_id())
                .ok_or_else(|| format!("collector cannot retain {:?}", input.kind))?
                .clone();
            let mut request = CollectedLaneRequest::new(
                lane_name,
                *member,
                ctx.derived_lanes().clone(),
                descriptor,
                ctx.derived_data_retention(),
            );
            if input.kind == PortKind::of::<Word>() {
                let store_config = if let Some(persistent) = ctx.derived_word_cache(*member) {
                    LiveStoreConfig {
                        directory: persistent.directory.clone(),
                        persistence: Some(persistent.clone()),
                        ..LiveStoreConfig::default()
                    }
                } else {
                    LiveStoreConfig::default()
                };
                request = request.with_options(CollectedWordLaneOptions::new(
                    store_config,
                    input.word_display_format.clone(),
                ));
            }
            let adapter = collected_payloads
                .adapter_by_type_id(input.kind.type_id())
                .ok_or_else(|| {
                    format!(
                        "collected payload '{}' ({}) has no ingestion adapter",
                        input.kind.name(),
                        request.payload().stable_id()
                    )
                })?;
            let ingestor = adapter.create_ingestor(request).map_err(|error| {
                format!(
                    "collector adapter for '{}' could not create '{}': {error}",
                    input.kind.name(),
                    lane_name
                )
            })?;
            collector = collector.with_ingestor(ingestor);
        }
        Ok(Box::new(collector))
    }

    fn default_lane_names(resolved: &ResolvedInputs) -> Vec<(usize, String)> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        resolved
            .members(0)
            .into_iter()
            .map(|(member, input)| {
                let count = counts.entry(input.source.clone()).or_default();
                *count += 1;
                let name = if *count == 1 {
                    input.source.clone()
                } else {
                    format!("{} ({count})", input.source)
                };
                (member, name)
            })
            .collect()
    }
}

impl RuntimeBuilder for DataCollectorBuilder {
    fn is_sink(&self) -> bool {
        true
    }

    fn is_data_collector(&self) -> bool {
        true
    }

    fn collected_lane_names(
        &self,
        _state: &Value,
        resolved: &ResolvedInputs,
    ) -> Vec<(usize, String)> {
        Self::default_lane_names(resolved)
    }

    fn accepted_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        vec![
            PortKind::of::<Sample>(),
            PortKind::of::<Word>(),
            PortKind::of::<Trigger>(),
            PortKind::of::<NumberSample>(),
            PortKind::of::<TextSample>(),
        ]
    }

    fn offered_kinds(&self, _socket: &Socket, _state: &Value) -> Vec<PortKind> {
        Vec::new()
    }

    fn input_port(
        &self,
        _socket: &Socket,
        member_index: usize,
        _state: &Value,
        _kind: PortKind,
    ) -> Option<String> {
        Some(format!("in{member_index}"))
    }

    fn output_port(&self, _socket: &Socket, _state: &Value, _kind: PortKind) -> Option<String> {
        None
    }

    fn input_required(&self, _socket: &Socket, _state: &Value) -> bool {
        false
    }

    fn build(
        &self,
        name: &str,
        _state: &Value,
        resolved: &ResolvedInputs,
        ctx: &mut CompileCtx,
    ) -> Result<Box<dyn ProcessNode>, String> {
        let mut collected_payloads = CollectedPayloadRegistry::new();
        signal_processing::register_builtin_collected_payload_adapters(&mut collected_payloads)
            .expect("built-in collected payload adapters must be valid");
        Self::build_with_lane_names(
            name,
            resolved,
            &Self::default_lane_names(resolved),
            &collected_payloads,
            ctx,
        )
    }
}
