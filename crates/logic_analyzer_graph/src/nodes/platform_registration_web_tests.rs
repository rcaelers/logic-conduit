use egui::Pos2;

use node_graph::{NodeDef, NodeGraphWidget};

use crate::BuilderRegistry;
use crate::nodes::{
    CsvWriter, DsLogicU3Pro16, DslFileSource, FileWriter, SigrokFileSource, TestLiveCaptureSource,
    TextFileWriter, build_registry,
};

#[test]
fn browser_discovers_every_platform_sensitive_node_and_builder() {
    let names = [
        TestLiveCaptureSource::name(),
        DslFileSource::name(),
        DsLogicU3Pro16::name(),
        SigrokFileSource::name(),
        FileWriter::name(),
        TextFileWriter::name(),
        CsvWriter::name(),
    ];
    let mut graph = NodeGraphWidget::new(build_registry());
    let builders = BuilderRegistry::standard();

    for (index, name) in names.into_iter().enumerate() {
        assert!(
            graph
                .add_node_at(name, Pos2::new(index as f32 * 10.0, 0.0))
                .is_some(),
            "missing browser node definition for {name}"
        );
        assert!(
            builders.get(name).is_some(),
            "missing browser runtime builder for {name}"
        );
    }
}
