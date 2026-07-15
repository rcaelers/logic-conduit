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
    use node_graph::NodeGraphWidget;

    use super::*;
    use crate::compiler::{BuilderRegistry, lower};
    use crate::nodes::{build_registry, populate_startup};

    #[test]
    fn native_hardware_example_graphs_load_and_lower() {
        for json in [
            include_str!("../../../../graphs/pi5_u3pro16_spi_decode.json"),
            include_str!("../../../../graphs/u3pro16_spi_decode.json"),
        ] {
            let graph: node_graph::GraphState = serde_json::from_str(json).unwrap();
            let mut widget = NodeGraphWidget::new(build_registry());
            widget.set_graph(graph);
            lower(widget.graph(), &BuilderRegistry::standard()).unwrap();
        }
    }

    #[test]
    fn dsl_source_path_found_by_def_name_after_rename() {
        let mut widget = NodeGraphWidget::new(build_registry());
        populate_startup(&mut widget);
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
