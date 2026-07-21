use std::collections::HashMap;

use node_graph::{NodeDef, NodeTypeRegistry};

use super::sinks::{
    CsvWriter, CsvWriterBuilder, FileWriter, FileWriterBuilder, TextFileWriter,
    TextFileWriterBuilder,
};
use super::sources::{
    DsLogicU3Pro16, DsLogicU3Pro16Builder, DslFileSource, FileSourceBuilder, SigrokFileSource,
    SigrokFileSourceBuilder,
};
#[cfg(any(test, feature = "test-support"))]
use super::sources::{TestLiveCaptureSource, TestLiveCaptureSourceBuilder};
use crate::RuntimeBuilder;

pub(crate) fn register_nodes(registry: &mut NodeTypeRegistry) {
    #[cfg(any(test, feature = "test-support"))]
    registry.register::<TestLiveCaptureSource>();
    registry.register::<DslFileSource>();
    registry.register::<DsLogicU3Pro16>();
    registry.register::<SigrokFileSource>();
    registry.register::<FileWriter>();
    registry.register::<TextFileWriter>();
    registry.register::<CsvWriter>();
}

pub(crate) fn register_builders(builders: &mut HashMap<String, Box<dyn RuntimeBuilder>>) {
    #[cfg(any(test, feature = "test-support"))]
    builders.insert(
        TestLiveCaptureSource::name().into(),
        Box::new(TestLiveCaptureSourceBuilder),
    );
    builders.insert(DslFileSource::name().into(), Box::new(FileSourceBuilder));
    builders.insert(
        DsLogicU3Pro16::name().into(),
        Box::new(DsLogicU3Pro16Builder),
    );
    builders.insert(
        SigrokFileSource::name().into(),
        Box::new(SigrokFileSourceBuilder),
    );
    builders.insert(FileWriter::name().into(), Box::new(FileWriterBuilder));
    builders.insert(
        TextFileWriter::name().into(),
        Box::new(TextFileWriterBuilder),
    );
    builders.insert(CsvWriter::name().into(), Box::new(CsvWriterBuilder));
}

#[cfg(test)]
mod tests {
    use egui::Pos2;
    use node_graph::NodeGraphWidget;

    use super::*;
    use crate::BuilderRegistry;
    use crate::nodes::build_registry;

    #[test]
    fn browser_registers_every_platform_sensitive_native_node_and_builder() {
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
}
