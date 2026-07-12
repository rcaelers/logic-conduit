# TODO

## Logic-analyzer viewer

- Add global and per-lane height zoom, using modifier + scroll-wheel input.
- Replace temporary UART name-based lane pairing with generic derived-lane
  group/track metadata; see [Decoder View Lane Design](docs/DECODER_VIEW_LANE_DESIGN.md).
- Move all concrete-node/protocol handling out of `logic_analyzer_viewer`,
  the generic Viewer node, and generic compiler/node infrastructure. Keep it
  in node-specific UI definitions/builders and DSL runtime nodes, passing
  presentation requirements through the generic lane-group contract.
- Support displaying multiple capture sources in the logic-analyzer viewer.
- Let the viewer select which source is visible while the one-source display restriction remains.
- Add time offsets and alignment controls for sources, including a clear shared time-base model.
- Display live-source snapshots in the viewer through the same `CaptureDataSource` boundary used by file captures.

Related design: [Logic Analyzer Viewer Design](docs/LOGIC_ANALYZER_VIEWER_DESIGN.md) and [Pipeline Design](docs/PIPELINE_DESIGN.md).

## Capture sources

- Make file and live sources first-class capture providers, rather than having the app select source types explicitly.
- Persist/reload live-capture snapshots where appropriate so they can be indexed and revisited.
- Extend Sigrok support beyond v2 digital `logic-*` data (analog channels and newer format versions).

## Graph and runtime

- Define how several source clocks and trigger positions map onto the shared viewer timeline.
- Add graph-level source grouping/alignment metadata and preserve it in saved graphs.
