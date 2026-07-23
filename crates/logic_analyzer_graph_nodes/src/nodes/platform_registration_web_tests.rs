use egui::Pos2;

use logic_analyzer_graph::host::BuilderRegistry;
use node_graph::NodeGraphWidget;

use crate::nodes::node_name;
use crate::test_support::build_registry;

#[test]
fn browser_discovers_every_platform_sensitive_node_and_builder() {
    let stable_ids = [
        "org.logicconduit.graph-node.test-live-capture-source/v1",
        "org.logicconduit.graph-node.dsl-file-source/v1",
        "org.logicconduit.graph-node.dslogic-u3pro16/v1",
        "org.logicconduit.graph-node.sigrok-file-source/v1",
        "org.logicconduit.graph-node.file-writer/v1",
        "org.logicconduit.graph-node.text-file-writer/v1",
        "org.logicconduit.graph-node.csv-writer/v1",
    ];
    let mut graph = NodeGraphWidget::new(build_registry());
    let builders = BuilderRegistry::standard();

    for (index, stable_id) in stable_ids.into_iter().enumerate() {
        let name = node_name(stable_id);
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
