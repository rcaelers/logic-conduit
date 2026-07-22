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
- Make sampling-point overlays passive viewer data. Move clock-edge selection, qualifier
  evaluation, and sampled-value lookup out of `logic_analyzer_viewer` into the owning concrete
  runtime node or neutral processing infrastructure. Pass explicit, generic sampling-point
  records and presentation metadata to the viewer so an overlay reflects produced data rather
  than the viewer interpreting raw channels before the node has run.

### Plugin-extensible viewer payloads

`ViewerLaneKind`, `ViewerValueKind`, `DerivedLaneData`, `LaneBuffer`, the synthetic Viewer
socket, and `ViewerBuilder` currently form a closed list of payloads the viewer understands.
Number and text viewing may remain as the temporary implementation, but the viewer must become
open to plugin-defined socket and value types. A plugin-defined camera frame, for example, must
be able to register timestamp extraction, storage/sampling, a thumbnail lane, and its default
presentation without editing a generic crate.

- Extend the collected-payload identity registry keyed by the existing open
  `PortKind`/payload `TypeId` with a typed ingest factory, timeline semantics, storage/query
  adapter, default badge/group, and renderer. Registration must compose with
  `register_payload::<T>()`; adding a type must not require a match arm in
  `logic_analyzer_graph`, `signal_processing`, or `logic_analyzer_viewer`.
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

Related design: [Plugin-Extensible Collected Payload Design](docs/PLUGIN_EXTENSIBLE_PAYLOAD_DESIGN.md), [Logic Analyzer Viewer Design](docs/LOGIC_ANALYZER_VIEWER_DESIGN.md), and [Pipeline Design](docs/PIPELINE_DESIGN.md).

## Capture sources

### Consolidate wasm stand-ins behind processing platform facades

- Make `logic_analyzer_graph` compile the same concrete node definitions and runtime builders on
  native and wasm. It must describe node state, ports, and presentation contracts without knowing
  that a wasm runtime is synthetic or that a native runtime uses USB/filesystem resources.
- Move selection of real versus synthetic source and sink implementations into whole-file
  platform facades owned by `logic_analyzer_processing`. The U3Pro16 facade selects the USB-backed
  implementation natively and a synthetic implementation on wasm; file-source facades select
  native readers or deterministic in-memory captures; writer facades select filesystem writers or
  discard sinks.
- Prefer a platform-neutral factory or wrapper with one constructor/configuration surface. Use a
  type re-export alias only where the native and wasm implementations genuinely satisfy the same
  API; do not force hardware-only control methods onto synthetic implementations merely to make an
  alias compile.
- Pass synthetic capture presentation and runtime capabilities back through explicit processing
  metadata/contracts. Remove `builder_wasm.rs`, synthetic-presentation helpers, and target-specific
  builder registration from `logic_analyzer_graph` once the processing facade owns those choices.
- Keep target selection in one processing `platform` boundary per capability and add native/wasm
  catalog, port-schema, state-option, and lowering-parity tests.

- Implement the dependency-ordered delivery plan in
  [Live Capture and Trigger Control](docs/LIVE_CAPTURE_TRIGGER_DESIGN.md). Continue with Phase 13 and do
  not begin a later phase until the preceding completion gate passes:
  1. **Minimal authoritative store — complete:** sequential staging, committed-prefix cursors,
     finalization, byte-exact replay, bounded memory, and slow-reader isolation are implemented.
  2. **Immediate-capture application integration — complete:** generic feature discovery,
     coordinator, title-bar Start/Stop and status, orderly drain, and graph read-only state are
     implemented using the fake provider.
  3. **Growing live waveform — complete:** incremental summaries, growing exact and summary
     timeline queries, viewer attachment, Follow Newest, Pause Display, and Go Live are implemented
     and covered with paced fake-capture tests.
  4. **Independent live graph analysis — complete:** a provider-owned source process consumes an
     independent committed-store cursor, the fixed graph publishes progress and lag, and throttled
     catch-up tests prove acquisition isolation and finite-reference derived-output equivalence.
  5. **Finalized-session Run replay — complete:** finalized stores retain their source node and
     captured source factory, Run creates fresh derived stores through explicit node-ID overrides,
     and byte-equal tests prove replay performs no provider discovery or device operation.
  6. **Portable simple triggering — complete:** neutral conditions, lane controls,
     recording-origin gating, migration diagnostics, trigger markers, and deterministic
     fake-trigger tests are implemented.
  7. **Provider-neutrality conformance — complete:** the device-buffered fake, explicit delivery
     and setting capabilities, shared provider/coordinator/viewer/analysis/replay/trigger suite,
     plug-in registration proof, and generic-source architecture guard are implemented.
  8. **U3Pro16 device-buffered acquisition — complete:** concrete state migration,
     negotiation/lowering, trigger-header position, lossless upload, fixture coverage, and an
     ignored hardware test are implemented.
  9. **U3Pro16 host streaming and sustained ingest — complete:** the streaming profile, actual-link
     tuple validation, integrity reporting, bounded file-backed summaries, and measured ingest
     benchmark are implemented.
  10. **Capture policies and health controls — complete:** finite completion,
      rolling-retention policy and safe-boundary planning, trigger placement, timeout and one-shot
      controls, capacity estimates, telemetry, persisted effective plans, and reclamation-safety
      tests are implemented.
  11. **Recovery and session ownership — complete:** checksummed commit-boundary recovery,
      interruption-safe bounded reclamation, durable outcomes, incomplete-session presentation,
      pinning, explicit keep/discard cleanup, configurable recent-session ownership, reopening,
      and replay are implemented.
  12. **Export — complete:** durable timeline metadata, pinned background DSL/portable raw export,
      bounded streaming, progress/cancellation, temporary destination files, trigger-position
      preservation, and explicit format capabilities are implemented.
  13. **Extended workflows:** keep the stable subphase numbers below and complete each focused gate
      before starting the next one:
      - **13.1 Configuration epochs — complete:** recording-time hot configuration switches at an
        explicit durable-source/analysis-time boundary; pending and resolved graph revisions are
        durable, interrupted attempts recover visibly, and structural/source/acquisition edits are
        deferred.
      - **13.2 Advanced-trigger contract — complete:** the provider-neutral staged/counted and
        registered-predicate schema, typed programs, structured validation, capability
        negotiation, simple-trigger bridge, and concrete-owner edit-routing boundary are
        implemented without device-specific cases in generic UI/compiler/runtime code.
      - **13.3 Advanced Triggers panel — complete:** pure trigger-configuration discovery,
        schema-driven neutral editing, concrete-owner persistence and migration diagnostics, and
        one-program interoperability between lane controls and the panel are implemented on native
        and wasm without acquisition-dependent UI state.
      - **13.4 Concrete advanced-trigger execution — complete:** supported programs lower in each
        owning source feature; the deterministic provider executes staged programs across chunk
        boundaries, and U3Pro16 hardware lowering has checked multi-stage packet coverage.
      - **13.5 Repeated and segmented acquisition:** introduce frame identity, per-frame origin and
        trigger metadata, bounded storage, replay, and viewer navigation.
      - **13.6 Live search and measurements:** operate over committed raw/derived prefixes with
        explicit coverage and lag.
      - **13.7 Notifications and power integration:** add host capabilities for capture lifecycle,
        integrity, storage, and sleep inhibition without platform conditionals in consumers.
      - **13.8 Automation:** expose the same validated coordinator commands and outcomes through a
        UI-independent service.
      - **13.9 Source synchronization:** add external trigger/clock contracts and shared-timeline
        alignment after multi-source viewer support is defined.
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
