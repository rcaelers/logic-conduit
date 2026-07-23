//! Built-in graph-node runtime builder catalog.

use std::collections::HashMap;

use crate::RuntimeBuilder;

pub(crate) fn standard_builders() -> HashMap<String, Box<dyn RuntimeBuilder>> {
    let mut builders: HashMap<String, Box<dyn RuntimeBuilder>> = HashMap::new();

    builders.insert(
        crate::compiler::DATA_COLLECTOR_BUILDER.into(),
        Box::new(crate::compiler::DataCollectorBuilder),
    );

    super::registry_platform::register_builders(&mut builders);

    for registration in super::graph_node_registrations() {
        assert!(
            builders
                .insert(registration.name().to_owned(), registration.builder())
                .is_none(),
            "graph-node inventory builder '{}' conflicts with an explicit catalog entry",
            registration.name()
        );
    }
    builders
}
