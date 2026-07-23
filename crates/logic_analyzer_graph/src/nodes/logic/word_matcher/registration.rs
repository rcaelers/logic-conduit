inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::WordMatcher,
        super::builder::WordMatcherBuilder,
    >("org.logicconduit.graph-node.word-matcher/v1").requiring_payloads(&[
        "org.logicconduit.digital-sample/v1",
        "org.logicconduit.trigger/v1",
        "org.logicconduit.word/v1",
    ])
}
