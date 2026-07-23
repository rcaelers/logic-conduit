inventory::submit! {
    crate::GraphNodeRegistration::runnable::<
        super::definition::StringFormatter,
        super::builder::FormatterBuilder,
    >("org.logicconduit.graph-node.string-formatter/v1").requiring_payloads(&[
        "org.logicconduit.number-sample/v1",
        "org.logicconduit.text-sample/v1",
    ])
}
