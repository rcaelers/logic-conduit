# TODO

## Logic-analyzer viewer

- Add global and per-lane height zoom, using modifier + scroll-wheel input.
- Replace temporary UART name-based lane pairing with generic derived-lane
  group/track metadata; see [Decoder View Lane Design](docs/DECODER_VIEW_LANE_DESIGN.md).
- Move all concrete-node/protocol handling out of `logic_analyzer_viewer`,
  the generic Viewer node, and generic compiler/node infrastructure. Keep it
  in node-specific UI definitions/builders and `signal_processing` runtime nodes, passing
  presentation requirements through the generic lane-group contract.
- Support displaying multiple capture sources in the logic-analyzer viewer.
- Let the viewer select which source is visible while the one-source display restriction
  remains.
- Add time offsets and alignment controls for sources, including a clear shared time-base
  model.
- Display live-source snapshots in the viewer through the same `CaptureDataSource` boundary
  used by file captures.

Related design: [Logic Analyzer Viewer Design](docs/LOGIC_ANALYZER_VIEWER_DESIGN.md) and [Pipeline Design](docs/PIPELINE_DESIGN.md).

## Capture sources

- Make file and live sources first-class capture providers, rather than having the app select
  source types explicitly.
- Persist/reload live-capture snapshots where appropriate so they can be indexed and revisited.
- Extend Sigrok support beyond v2 digital `logic-*` data (analog channels and newer format versions).

## Indexed derived data

- Run the ignored release-mode writer differential and golden graph tests against the complete
  reference capture; record output sizes and hashes and ensure temporary artifacts are contained.
- Add read-only derived-cache inventory/usage reporting to complement the existing clear-cache
  commands. Active mapped entries must remain pinned and visible as retained.
- Profile egui update, indexed sampling, lane-lock duration, repaint cadence, and input latency
  while decoding a complete capture; add focused regressions for any reproduced stall.
- Optionally profile the indexed-store append pipeline toward the sub-50-second full-cache stretch
  target. Optimize only measured builder/encode/write phases while preserving fingerprints,
  bounded RSS, query latency, and cancellation.
- Audit native `DerivedLaneData::Annotations` paths after plugin/wasm compatibility is confirmed;
  remove only duplicate native retention while preserving wasm, explicit in-memory mode, and
  storage-failure fallback.

## Graph and runtime

- Define how several source clocks and trigger positions map onto the shared viewer timeline.
- Add graph-level source grouping/alignment metadata and preserve it in saved graphs.
- Prepare `node-graph` for an eventual separate repository: replace workspace-inherited
  package/dependency metadata when extraction is scheduled, move its documentation and
  examples with the crate, add standalone CI, and make native file-dialog integration an
  optional feature or host capability.
