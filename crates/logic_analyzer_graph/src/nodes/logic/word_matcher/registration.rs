inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::WordMatcher,
        super::builder::WordMatcherBuilder,
    >("org.logicconduit.graph-node.word-matcher/v1")
}
