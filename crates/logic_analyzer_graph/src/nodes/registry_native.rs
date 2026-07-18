use std::collections::HashMap;

use node_graph::{NodeDef, NodeTypeRegistry};

use super::csv_writer::{CsvWriter, CsvWriterBuilder};
use super::dslogic_u3pro16::{DsLogicU3Pro16, DsLogicU3Pro16Builder};
use super::file_source::{DslFileSource, DslFileSourceState};
use super::file_source_builder::FileSourceBuilder;
use super::file_writer::FileWriter;
use super::file_writer_builder::FileWriterBuilder;
use super::sigrok_file_source::{
    SigrokFileSource, SigrokFileSourceBuilder, SigrokFileSourceState,
};
use super::text_file_writer::{TextFileWriter, TextFileWriterBuilder};
use crate::compiler::RuntimeBuilder;

pub(super) fn register_nodes(registry: &mut NodeTypeRegistry) {
    registry.register::<DslFileSource>();
    registry.register::<DsLogicU3Pro16>();
    registry.register::<SigrokFileSource>();
    registry.register::<FileWriter>();
    registry.register::<TextFileWriter>();
    registry.register::<CsvWriter>();
}

pub(super) fn register_builders(builders: &mut HashMap<String, Box<dyn RuntimeBuilder>>) {
    builders.insert("DSL File Source".into(), Box::new(FileSourceBuilder));
    builders.insert(
        "DSLogic U3Pro16".into(),
        Box::new(DsLogicU3Pro16Builder),
    );
    builders.insert(
        "Sigrok File Source".into(),
        Box::new(SigrokFileSourceBuilder),
    );
    builders.insert("File Writer".into(), Box::new(FileWriterBuilder));
    builders.insert(
        "Text File Writer".into(),
        Box::new(TextFileWriterBuilder),
    );
    builders.insert("CSV Writer".into(), Box::new(CsvWriterBuilder));
}

/// The single file-backed source currently displayed by the logic-analyzer
/// view. Multiple sources and live-capture snapshots are future work.
pub enum CaptureFileSource {
    Dsl(String),
    Sigrok(String),
}

pub fn capture_file_source(graph: &node_graph::GraphState) -> Option<CaptureFileSource> {
    graph.nodes.values().find_map(|node| {
        if node.def_name() == DslFileSource::name() {
            let state = serde_json::from_value::<DslFileSourceState>(node.state.clone()).ok()?;
            (!state.file.value.is_empty()).then_some(CaptureFileSource::Dsl(state.file.value))
        } else if node.def_name() == SigrokFileSource::name() {
            let state = serde_json::from_value::<SigrokFileSourceState>(node.state.clone()).ok()?;
            (!state.file.value.is_empty()).then_some(CaptureFileSource::Sigrok(state.file.value))
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use egui::Pos2;
    use node_graph::{NodeDef, NodeGraphWidget};
    use signal_processing::{CaptureChannelId, CaptureDataDelivery, SimpleTriggerCondition};

    use super::*;
    use crate::compiler::{
        BuilderRegistry, LiveCaptureEdit, apply_live_capture_edit, discover_live_capture_feature,
        lower,
    };
    use crate::nodes::{U3Pro16State, build_registry, test_graphs};

    #[test]
    fn native_hardware_source_registers_and_lowers() {
        let mut widget = NodeGraphWidget::new(build_registry());
        let source = widget
            .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
            .expect("native hardware source should be registered");
        widget.graph_mut().nodes.get_mut(&source).unwrap().outputs[0].show_in_view = true;

        let compiled = lower(widget.graph(), &BuilderRegistry::standard()).unwrap();
        assert!(
            compiled
                .nodes
                .iter()
                .any(|node| node.builder == DsLogicU3Pro16::name())
        );
    }

    #[test]
    fn buffered_hardware_feature_lowers_opaque_channels_and_portable_trigger_edits() {
        let mut widget = NodeGraphWidget::new(build_registry());
        let source = widget
            .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
            .unwrap();
        let streaming =
            discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
                .unwrap()
                .expect("stream mode should expose a live feature");
        assert_eq!(
            streaming.capabilities().data_delivery(),
            CaptureDataDelivery::DuringAcquisition
        );
        let mut state = serde_json::from_value::<U3Pro16State>(
            widget.graph().nodes[&source].state.clone(),
        )
        .unwrap();
        state.mode.select("Buffer");
        state.channels.enabled.fill(false);
        for channel in [0, 2, 9] {
            state.channels.enabled[channel] = true;
        }
        widget.graph_mut().nodes.get_mut(&source).unwrap().state =
            serde_json::to_value(state).unwrap();

        let builders = BuilderRegistry::standard();
        let feature = discover_live_capture_feature(widget.graph(), &builders)
            .unwrap()
            .expect("buffer mode should expose the concrete live feature");
        assert_eq!(feature.source_node, source);
        assert_eq!(
            feature.channels(),
            [
                CaptureChannelId::new("u3pro16:input:0"),
                CaptureChannelId::new("u3pro16:input:2"),
                CaptureChannelId::new("u3pro16:input:9"),
            ]
        );
        assert_eq!(
            feature.capabilities().data_delivery(),
            CaptureDataDelivery::BufferedUpload
        );
        assert!(
            feature
                .capabilities()
                .supports(feature.channels(), feature.sample_rate_hz())
        );

        let edited = apply_live_capture_edit(
            widget.graph(),
            &builders,
            source,
            &LiveCaptureEdit::SetSimpleTrigger {
                channel_id: CaptureChannelId::new("u3pro16:input:2"),
                condition: SimpleTriggerCondition::Falling,
            },
        )
        .unwrap();
        widget.graph_mut().nodes.get_mut(&source).unwrap().state = edited;
        let feature = discover_live_capture_feature(widget.graph(), &builders)
            .unwrap()
            .unwrap();
        assert_eq!(
            feature.simple_trigger_channels()[1].condition,
            SimpleTriggerCondition::Falling
        );
    }

    #[test]
    fn buffered_hardware_discovery_rejects_an_unsupported_active_tuple() {
        let mut widget = NodeGraphWidget::new(build_registry());
        let source = widget
            .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
            .unwrap();
        let mut state = serde_json::from_value::<U3Pro16State>(
            widget.graph().nodes[&source].state.clone(),
        )
        .unwrap();
        state.mode.select("Buffer");
        state.sample_rate.select("1 GHz");
        state.channels.enabled.fill(true);
        widget.graph_mut().nodes.get_mut(&source).unwrap().state =
            serde_json::to_value(state).unwrap();

        let error = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
            .err()
            .expect("wide 1 GHz buffered capture must be rejected before opening hardware");

        assert!(error.message.contains("outside this mode"));
    }

    #[test]
    fn streaming_hardware_discovery_rejects_a_tuple_unsupported_on_every_link() {
        let mut widget = NodeGraphWidget::new(build_registry());
        let source = widget
            .add_node_at(DsLogicU3Pro16::name(), Pos2::ZERO)
            .unwrap();
        let mut state = serde_json::from_value::<U3Pro16State>(
            widget.graph().nodes[&source].state.clone(),
        )
        .unwrap();
        state.mode.select("Stream");
        state.sample_rate.select("1 GHz");
        state.channels.enabled.fill(false);
        state.channels.enabled[0] = true;
        state.channels.enabled[3] = true;
        widget.graph_mut().nodes.get_mut(&source).unwrap().state =
            serde_json::to_value(state).unwrap();

        let error = discover_live_capture_feature(widget.graph(), &BuilderRegistry::standard())
            .err()
            .expect("four-input 1 GHz stream must be rejected before opening hardware");

        assert!(error.message.contains("High Speed"));
        assert!(error.message.contains("SuperSpeed"));
    }

    #[test]
    fn dsl_source_path_found_by_def_name_after_rename() {
        let mut widget = NodeGraphWidget::new(build_registry());
        test_graphs::populate_startup(&mut widget);
        let source_id = *widget
            .graph()
            .nodes
            .iter()
            .find(|(_, node)| node.def_name() == DslFileSource::name())
            .map(|(id, _)| id)
            .unwrap();
        widget.graph_mut().nodes.get_mut(&source_id).unwrap().title = "My capture".to_owned();
        assert_eq!(
            match capture_file_source(widget.graph()) {
                Some(CaptureFileSource::Dsl(path)) => path,
                _ => String::new(),
            },
            ""
        );
    }
}
