# TODO

## Logic-analyzer viewer

- Add global and per-lane height zoom, using modifier + scroll-wheel input.
- Support displaying multiple capture sources in the logic-analyzer viewer.
- Let the viewer select which source is visible while the one-source display restriction
  remains.
- Add time offsets and alignment controls for sources, including a clear shared time-base
  model.
- Display live-source snapshots in the viewer through the same `CaptureDataSource` boundary
  used by file captures.

### Plugin-extensible viewer payloads

`ViewerLaneKind`, `ViewerValueKind`, `DerivedLaneData`, `LaneBuffer`, the synthetic Viewer
socket, and `ViewerBuilder` currently form a closed list of payloads the viewer understands.
Number and text viewing may remain as the temporary implementation, but the viewer must become
open to plugin-defined socket and value types. A plugin-defined camera frame, for example, must
be able to register timestamp extraction, storage/sampling, a thumbnail lane, and its default
presentation without editing a generic crate.

- Write the viewer-payload extension design before changing the implementation. Preserve the
  existing crate boundaries, platform-neutral API, bounded rendering rules, and the invariant
  that plugin code is never called while a derived-lane lock is held.
- Add a viewer-payload registry keyed by the existing open `PortKind`/payload `TypeId`. Extend
  `PluginContext` with one registration operation that associates a plugin payload type with
  its typed ingest factory, timeline semantics, storage/query adapter, default badge/group, and
  renderer. Registration must compose with `register_payload::<T>()`; adding a type must not
  require a match arm in `logic_analyzer_graph`, `signal_processing`, or
  `logic_analyzer_viewer`.
- Replace the Viewer's hardcoded accepted-kind list and the synthetic socket's type-name list
  with registry-driven negotiation or a true viewable-payload wildcard. Keep unsupported output
  types out of the View panel, and report a clear compile error if a saved or explicitly wired
  graph requests viewing for a payload whose adapter is unavailable.
- Replace `ViewerLaneKind`/`LaneBuffer` dispatch with an object-safe lane-ingest boundary created
  by a typed registration factory. The factory must still construct the correctly typed runtime
  `PortSchema` and drain the corresponding `InputPort`, while the generic `ViewerSink` only
  schedules lanes, applies backpressure/retention policy, and publishes progress.
- Replace the closed `DerivedLaneData` and `ViewerValueKind` representation with registered lane
  data/query handles. Separate typed append state from immutable UI snapshots so arbitrary
  values do not require conversion to strings or integers. Each adapter supplies timestamp/span
  extraction, timeline extent, bounded visible-window sampling, dense/activity fallback, and an
  optional indexed or custom storage capability.
- Generalize `ViewerLaneFrame` and `ViewerLaneRenderer` beyond word-style annotation formatting.
  A renderer must receive a bounded, type-safe-or-contract-checked snapshot plus a restricted
  drawing context containing row geometry, clipping, time transforms, theme information, and
  interaction hooks. This must support custom content such as camera thumbnails without giving
  plugins access to `LogicAnalyzerViewer` internals or locked runtime storage.
- Retain reusable built-in adapters for digital signals, words, triggers, numbers, and text, but
  register them through the same public mechanism plugins use. Default singleton presentation,
  cursor snapping, timeline fitting, row activity, retention, and native/wasm behavior must be
  capabilities of the registered adapter rather than exhaustive matches over built-in variants.
- Define saved-graph compatibility for Viewer sockets and `show_in_view` outputs. Migrate legacy
  built-in lanes explicitly, preserve their visual behavior, and show a user-visible warning for
  missing plugin payload/presentation registrations.
- Add an example plugin payload such as `CameraFrame { timestamp_ns, image }` with a custom socket,
  source node, bounded in-memory sampler, and thumbnail renderer. Use it as the end-to-end proof
  that a new value type is viewable through both an explicit Viewer connection and the View panel
  without modifying generic source files.
- Add architecture and contract tests covering plugin registration, duplicate/missing adapters,
  typed channel construction, auto-view negotiation, retention and dense snapshots, timeline
  extent, renderer lock release, native/wasm compilation, and removal of hardcoded built-in type
  checks from the generic viewer path.

Related design: [Logic Analyzer Viewer Design](docs/LOGIC_ANALYZER_VIEWER_DESIGN.md) and [Pipeline Design](docs/PIPELINE_DESIGN.md).

## Capture sources

- Implement the dependency-ordered delivery plan in
  [Live Capture and Trigger Control](docs/LIVE_CAPTURE_TRIGGER_DESIGN.md). Start with Phase 1 and do
  not begin a later phase until the preceding completion gate passes:
  1. **Provider lifecycle and deterministic fake:** add neutral session/lifecycle/chunk contracts,
     bounded delivery, prepared acquisition, and exact headless lifecycle tests. Do not modify the
     graph, UI, or viewer in this phase.
  2. **Minimal authoritative store:** add sequential staging, the minimal commit log, committed
     cursor, finalization, byte-exact replay, bounded memory, and slow-reader isolation.
  3. **Immediate-capture application integration:** add generic feature discovery, coordinator,
     title-bar Start/Stop and status, safe drain, and graph read-only state using the fake provider.
  4. **Growing live waveform:** add incremental summaries, growing timeline queries, Follow Newest,
     Pause Display, and Go Live using fake data.
  5. **Independent live graph analysis:** consume committed chunks through a lag-tolerant cursor and
     prove a slow graph neither blocks capture nor loses data.
  6. **Finalized-session Run replay:** add node-ID source overrides and byte-equal live/replay
     derived-output tests that prove no hardware is opened.
  7. **Portable simple triggering:** add neutral conditions, lane controls, recording-origin gating,
     a migration/diagnostic contract, and deterministic fake-trigger tests.
  8. **Provider-neutrality conformance:** add a deliberately different buffered fake provider with
     non-contiguous bank-qualified channels and pass both providers through the shared suite before
     integrating hardware.
  9. **U3Pro16 device-buffered acquisition:** add concrete state migration,
     negotiation/lowering, trigger-header position, lossless upload, fixture coverage, and an
     ignored hardware test.
  10. **U3Pro16 host streaming and sustained ingest:** add the streaming profile, tuple validation,
     integrity reporting, bounded-memory benchmarks, and measured optimization only where needed.
  11. **Capture policies and health controls:** add finite/rolling policy, trigger placement,
      timeout and one-shot controls, capacity estimates, telemetry, and reclamation tests.
  12. **Recovery and session ownership:** add commit-boundary recovery, incomplete-session handling,
      pinning, cleanup, and recent-session ownership.
  13. **Export:** add raw DSL/portable export first and capability-aware derived export afterward.
  14. **Extended workflows:** scope configuration epochs, advanced triggers, segmented acquisition,
      automation, synchronization, and related features as separate follow-up amendments rather
      than one implementation batch.
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
