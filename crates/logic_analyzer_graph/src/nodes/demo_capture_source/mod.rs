mod builder;
mod definition;

pub(crate) use builder::DemoCaptureSourceBuilder;
pub use definition::{DemoCaptureSource, DemoCaptureSourceState};

/// One raw capture row that can be shown independently of a pipeline run.
pub struct CapturePreviewSignal {
    pub index: usize,
    pub name: String,
    pub initial: bool,
    pub transitions: Vec<(f64, bool)>,
}

/// Returns the generated capture represented by `node` as the ten active raw
/// channels used by the demo graph. This is concrete source behavior; callers
/// only receive the generic preview contract.
pub(crate) fn capture_preview(node: &node_graph::Node) -> Option<Vec<CapturePreviewSignal>> {
    use node_graph::NodeDef;

    (node.def_name() == DemoCaptureSource::name()).then(|| {
        let channels = logic_analyzer_processing::DemoCaptureSource::preview_channels();
        (0..=8)
            .chain(std::iter::once(10))
            .map(|index| {
                let samples = &channels[index];
                CapturePreviewSignal {
                    index,
                    name: format!("Ch {index}"),
                    initial: samples.first().is_some_and(|sample| sample.value),
                    transitions: samples
                        .iter()
                        .skip(1)
                        .map(|sample| (sample.start_time_ns as f64 / 1_000.0, sample.value))
                        .collect(),
                }
            })
            .collect()
    })
}
