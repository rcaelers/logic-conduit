# Live Capture and Trigger Control — Proposed Future Design

> Status: proposed future design. The behavior under **Proposed future design** is not yet
> implemented. Actionable work is tracked in [TODO.md](../TODO.md).

This design adds interactive triggering, live waveform display, lossless host upload, live graph
processing, and post-capture re-analysis for hardware logic-analyzer sources such as the DSLogic
U3Pro16. It deliberately separates capture control from graph replay so the first implementation
can keep the graph fixed during acquisition without creating an architectural dead end.

## Current baseline

The U3Pro16 graph node already owns device settings and lowers to the UI-independent
`LogicAnalyzerSource<DsLogicU3Pro16>`. The portable `LogicCaptureConfig` and U3Pro16 packet builder
already represent hardware trigger stages. Today, however:

- the graph builder always supplies an empty trigger program;
- starting a graph run opens and starts the hardware source immediately;
- the driver consumes the trigger header but does not publish its trigger position or acquisition
  state;
- live raw channels are not exposed through the viewer's capture-query path;
- the USB source demultiplexes and sends directly into the graph, so downstream backpressure can
  delay acquisition;
- capture errors are logged but there is no typed status channel for the application;
- only file and demo sources provide raw viewer content; and
- there is no immutable captured session that Run can replay.

The existing live graph manager already supplies one important future invariant: hot changes and
branch restarts take effect from the current stream position and do not rewrite previously emitted
derived data.

## Proposed future design

### Scope of the first complete release slice

The first complete release slice supports exactly one live capture source and keeps the node graph
fixed from Start until the capture and downstream drain both finish. It provides:

- trigger controls beside each enabled physical input lane;
- one combined Start/Stop control in the Logic Analyzer panel title bar;
- one simple hardware trigger stage, AND-combining every non-ignored lane condition;
- composable immediate/triggered start, bounded retention, and manual/fixed completion policies;
- explicit Idle, Preparing, Armed, Triggered, Recording, Stopping, Complete, and Error states;
- bounded, sequential host upload of every data chunk returned by the device;
- live raw waveform queries while upload continues;
- live graph processing from an independent cursor over the uploaded stream; and
- Run-based re-analysis of the finalized capture with fresh derived-data stores.

This behavior is delivered through the small, dependency-ordered phases below. No phase needs to
implement the whole release slice at once. Multiple live sources, graph editing during capture,
repeated frames, advanced trigger stages, and capture export remain later phases. They use the same
contracts rather than replacing the first implementation.

### Terminology

| Term | Meaning |
| --- | --- |
| Capture session | One Start-to-Stop device acquisition, identified by a unique session ID. |
| Acquisition | Reading bytes from the hardware and uploading them to the host. |
| Armed | Hardware configuration is active and the device is waiting for its trigger. |
| Trigger point | Exact hardware sample position reported by the trigger header. |
| Recording origin | Sample treated as time zero for the logged capture; initially the trigger point. |
| Live capture store | Append-only raw staging store plus incremental waveform summaries. |
| Analysis cursor | Independent ordered reader that follows committed raw chunks and feeds the graph. |
| Captured session | Finalized immutable raw capture, metadata, trigger point, and graph revision. |
| Analysis run | Processing a live or finalized capture through the node graph. |
| Configuration epoch | Future timestamped graph configuration that applies only from one sample onward. |

“Upload” and “log” are intentionally different. Every chunk made available by the provider is
committed immediately to the live capture store; a device-buffered provider may not make chunks
available until its later upload phase. Logging begins at the recording origin. Device-provided
pre-trigger data can therefore be retained without pretending it belongs after the trigger.

### Architectural boundaries

| Crate | Responsibility |
| --- | --- |
| `signal_processing` | Generic session IDs/status, append-only live raw store, committed-prefix queries, trigger point metadata, analysis cursors, and finalized capture handles. No USB, node names, or UI. |
| `logic_analyzer_processing` | Portable analyzer control/events and concrete U3Pro16 USB behavior. It translates U3 trigger headers and chunks into the generic session contracts. |
| `logic_analyzer_graph` | U3Pro16 saved trigger state, generic live-source descriptors, trigger-state lowering, replay override lowering, and builder registration. Concrete U3 behavior stays in its feature directory. |
| `logic_analyzer_viewer` | Generic lane trigger icons, hit testing, live capture queries, and neutral trigger-edit events. It does not identify U3Pro16 or construct hardware trigger programs. |
| `logic_analyzer_ui` | Capture-session coordinator, Start/Stop state machine, graph lock, title-bar controls, status/toasts, and routing neutral edits between descriptors and widgets. It does not branch on node names. |
| `node_graph` | Generic read-only/edit-enabled mode during capture. It has no capture or trigger concepts. |

The U3Pro16 remains native-only as a complete registry/module boundary. Generic session, graph,
compiler, and viewer data models have no inline target conditionals.

### Driver-neutral invariants

U3Pro16 is the first implementation, not the definition of a logic analyzer. These invariants apply
to every generic contract in this design:

- the application, viewer, compiler core, session coordinator, and capture store never match a
  driver/model name or concrete node type;
- channel IDs are opaque and channel counts, banks, ordering, and masks are supplied explicitly;
- device discovery and capability negotiation occur per instance, after which one immutable typed
  plan controls the session;
- connection transport is opaque; USB, network, serial, local bridge, and future remote providers
  publish the same lifecycle/events;
- acquisition profiles describe observable semantics such as when data becomes available, who owns
  buffering, supported stop operations, and valid setting tuples instead of relying on universal
  `Streaming`/`Finite` assumptions;
- trigger controls expose a small common simple-trigger vocabulary, while richer trigger engines
  advertise their supported expression/schema capabilities and are lowered by the concrete feature;
- captured data uses a versioned canonical digital representation, so replay and export do not
  require the original hardware driver; and
- unsupported or unknown capabilities remain unavailable and produce structured diagnostics; no
  generic fallback guesses hardware behavior.

New analyzers integrate by registering a concrete graph feature and UI-independent processing
provider that implement these contracts. A source may use a dedicated graph node or a plugin-owned
node definition. Neither path requires editing generic crates.

The existing `LogicAnalyzerInfo`/`LogicCaptureConfig` boundary evolves accordingly. Generic launch
requests carry explicit channel IDs and typed standard settings, not a `u64` channel mask or fixed
sixteen-channel trigger planes. A processing provider discovers devices, opens a stable device
identity, reports instance capabilities, and prepares acquisition. Its concrete adapter lowers the
generic request into any fixed masks, register planes, transfer sizes, or vendor commands required
by that device. The graph feature delegates to this provider; those representations never reach the
application or reusable widgets.

Device selection is saved as a provider-owned selector plus an optional preferred stable identity,
not an operating-system path interpreted by the application. Resolution can report missing,
ambiguous, busy, incompatible, or available devices. The user must choose when several matching
devices exist; capture does not silently bind whichever enumerates first.

### Source discovery and presentation contract

`RuntimeBuilder` gains an optional, protocol-neutral `LiveCaptureFeature` contract. A concrete
builder returns `None` by default. The U3Pro16 builder implements the feature by adapting its
saved state and concrete processing driver; the application never constructs or identifies that
driver. The feature has three responsibilities:

- describe the source and its editable trigger controls;
- apply a neutral trigger edit to the concrete saved node state; and
- prepare an acquisition against a generic raw-store writer and status publisher.

Preparation returns an object-safe `PreparedAcquisition` handle with `start`, `request_stop`, and
`join` operations. Opening/configuring the device happens during preparation, while `start`
performs the final non-blocking arm operation. The handle publishes only generic
`LogicCaptureEvent` values and writes raw chunks through the supplied store contract. Its concrete
type remains inside the U3Pro16 graph/processing feature.

The presentation half of the feature returns a descriptor derived explicitly from node state:

```rust
pub struct LiveCaptureDescriptor {
    pub node_id: NodeId,
    pub title: String,
    pub device: DeviceBindingDescriptor,
    pub channels: Vec<LiveCaptureChannel>,
    pub simple_trigger: SimpleTrigger,
    pub capabilities: LiveCaptureCapabilities,
}

pub struct LiveCaptureChannel {
    pub id: CaptureChannelId,
    pub viewer_lane: ViewerLaneId,
    pub physical_label: String,
    pub name: String,
    pub simple_trigger: Option<SimpleTriggerChannelState>,
}

pub struct LiveCaptureCapabilities {
    pub transport_profiles: Vec<CaptureTransportProfile>,
    pub trigger_engine: TriggerEngineCapabilities,
    pub clock_sources: Vec<ClockSourceCapability>,
    pub commands: CaptureCommandCapabilities,
    pub trigger_io: TriggerIoCapabilities,
}
```

`CaptureChannelId` is an opaque, stable, serializable identifier owned by the source feature. It is
not an array index and need not be numeric or contiguous; a driver may use bank/pod-qualified IDs.
The concrete feature maps it to hardware inputs and graph outputs. The viewer lane, physical label,
and user-visible name are separate explicit fields. Generic code never derives identity from a
socket label, display name, or row order. The descriptor also lets the application create empty
lane headers as soon as a live node is added, before a device is opened.

The feature contract accepts a neutral edit such as
`SetSimpleTrigger { channel_id, condition }`. The concrete builder validates and rewrites
its own serialized state. The application and viewer do not deserialize `U3Pro16State`.

For live processing, compilation receives a source override for the same `NodeId` that follows an
analysis cursor over the live store. The hardware acquisition is therefore outside the graph's
backpressure domain, while the graph still sees the source node's normal output schema. The same
override mechanism later accepts an immutable finalized-session cursor for Run. This avoids two
independent ways of lowering a live source and keeps source substitution explicit.

Phase one rejects Start when zero or more than one live-source descriptor is present. This is a
clear capability error rather than silently picking the first source.

Transport profiles describe device-buffered and host-streamed acquisition separately. Each profile
declares its channel-count/sample-rate combinations, sample-depth limits, supported trigger kinds,
trigger-placement range, hardware encoding options, partial-upload behavior, and whether samples
become available during acquisition or only during a later upload phase. These are explicit
capabilities supplied by the concrete source feature, not rules inferred from a mode label.

Capabilities are queried for a particular discovered device instance. Static node defaults may
offer a useful initial configuration, but connection type, firmware revision, installed options,
channel banks, and current profile can change the valid choices. The feature exposes typed standard
capabilities to the coordinator and keeps additional device-specific properties in its concrete node
panel. Generic code does not consume integer option keys, untyped property bags, driver names, or
model-name conditionals.

### Saved simple-trigger state

`U3Pro16State` gains one trigger condition per physical input with a serde default of `Ignore`.
Old graph files therefore migrate explicitly to free-running capture without changing their
meaning. Trigger conditions belong to the hardware source node because they affect acquisition,
not merely presentation.

The first concrete source maps the common simple program to one of its current
`LogicTriggerStage` values:

- every enabled, non-ignored physical input contributes its selected condition;
- conditions are combined with `TriggerLogic::And`;
- plane 1 is unused;
- inversion is false, count is zero, and serial mode is false; and
- no configured conditions means immediate/free-running capture.

Its fixed-width channel planes are a concrete processing-adapter detail and do not cross the
`LiveCaptureFeature` boundary. Another analyzer may lower the same common simple conditions to a
different width or program representation.

The supported lane conditions are Ignore, Low, High, Rising, Falling, and Either. A primary click
opens a small condition menu; it does not rely only on cycling through icons. The icon and tooltip
show the actual selected condition. Input bindings and status-bar hints come from the existing
binding configuration rather than hardcoded shortcuts.

The later Triggers panel supplies advanced multi-stage programs through the same portable trigger
model. When an advanced program is active, lane icons summarize its selected stage rather than
maintaining a second, conflicting trigger program.

### Capture policy

Capture behavior is an explicit, generic policy rather than an implicit consequence of the Start
button. Orthogonal settings describe it without creating a separate implementation for each named
mode:

```rust
pub struct CapturePolicy {
    pub start: RecordingStart,
    pub trigger_placement: Option<TriggerPlacement>,
    pub retention_before_origin: RetentionPolicy,
    pub retention_after_origin: RetentionPolicy,
    pub completion: CompletionPolicy,
    pub trigger_timeout: Option<TriggerTimeout>,
}

pub enum RecordingStart {
    Immediate,
    Trigger,
}

pub enum TriggerPlacement {
    Fraction(CaptureFraction),
    SamplesBefore(u64),
    DurationBefore(Duration),
}

pub enum RetentionPolicy {
    Everything,
    RecentDuration(Duration),
    RecentBytes(u64),
    DeviceManaged,
}

pub enum CompletionPolicy {
    UntilStopped,
    DurationAfterOrigin(Duration),
    SamplesAfterOrigin(u64),
}

pub struct TriggerTimeout {
    pub after: Duration,
    pub action: TriggerTimeoutAction,
}

pub enum TriggerTimeoutAction {
    ContinueWaiting,
    Stop,
    ForceTrigger,
}
```

These settings compose into continuous, fixed-length, rolling-window, and triggered captures. A
triggered capture can retain a bounded window before the trigger and everything after it, while a
rolling capture can bound retention after its immediate origin as well. `DeviceManaged` is valid
only where the source controls the available history. The source descriptor reports which
combinations the hardware supports, and the application rejects unsupported combinations before
opening the device.

Trigger placement specifies how much of a finite capture window precedes the trigger; it is not
merely a viewer marker. The concrete feature converts percentage, sample, or duration input to the
device's aligned sample position and reports the effective value. A source may expose a freely
selectable placement for device-buffered acquisition and a fixed placement for host streaming.
The UI displays the effective pre/post-trigger split and does not imply that an unsupported value
was honored.

Every incoming chunk is transported and committed before it is made visible to consumers.
`Everything` keeps the complete committed prefix. A bounded retention policy permits reclamation
only after the trigger detector and every integrity check have processed that prefix. Reclamation
is recorded in the commit log, never presented as packet loss, and cannot remove pinned data.
`DeviceManaged` records the actual pre-trigger range delivered by hardware instead of promising a
host-side duration the device cannot supply.

Before Start, a capacity estimator shows the requested sample rate, enabled channels, estimated
uncompressed input rate, configured memory/disk budget, expected retained duration, and currently
available disk space. Compression estimates are labeled estimates and do not replace the
worst-case integrity check. A policy that cannot be sustained is rejected or requires an explicit
reduced-rate/channel choice; capture never silently changes settings.

### Negotiated acquisition plan

Device-buffered and host-streamed acquisition have different observable behavior and remain
distinct in the generic contract:

- **Device buffered:** sampling first accumulates in analyzer memory. Waveform bytes may be
  unavailable until the trigger, requested depth, or manual Stop completes, after which an Upload
  phase transfers them to the authoritative store. This path can support higher sample rates but
  has a hardware-depth limit.
- **Host streamed:** chunks are transferred and committed while sampling. This path enables a
  growing live waveform and longer captures, but its sustainable sample rate depends on link speed,
  enabled channel count, encoding, and host throughput.

Opening the device performs a capability handshake and produces an immutable `CapturePlan` before
arming. The plan records device identity, relevant firmware/logic revisions, transport/link class,
enabled physical channels, clock source, requested and effective sample rate, requested and aligned
sample depth, requested and effective trigger placement, encoding, expected raw rate, and supported
stop behavior. It is saved in session metadata and passed unchanged to the driver and store.

The plan is validated as a tuple. A sample rate that is legal for three channels may be illegal for
sixteen; an encoding may be available only with device buffering; advanced triggers may be limited
to one transport profile. If connection speed or device revision changes what is possible, Prepare
returns a structured incompatibility with suggested valid tuples. It never silently clamps one
field and leaves the graph showing the requested value.

For external-clock capture, the timebase is explicit. If the external frequency is known, metadata
contains that declared rate and timestamps can be expressed in time. If it is unknown, the capture
uses sample ordinals and the ruler displays samples; generic code does not reuse the internal-clock
rate as a guess.

The first concrete hardware feature advertises two profiles from its existing validated rate/depth
tables: high-rate device buffering and long-duration host streaming. Selectable trigger placement,
hardware run-length encoding, advanced staged triggers, and stop-with-partial-upload are associated
with the buffered profile only where supported. Streaming exposes its channel-count/link-speed rate
matrix and its fixed trigger placement. Physical clock or trigger pins become capabilities only
after their driver behavior is implemented and verified; connector presence alone is not enough.

Capture-policy edits are routed through the same neutral feature contract as lane trigger edits and
stored in the concrete source node state. Requested settings therefore survive graph save/load.
Negotiated effective settings belong to the captured session, because they describe one particular
device connection. Capture Now is a transient session override and is never serialized over the
saved recording-start policy.

### Capture-session state machine

```text
                    Start
 Idle / Complete ─────────► Preparing ─────► Armed
       ▲                       │                │
       │                       │ error          │ hardware trigger
       │                       ▼                ▼
       └────── Complete ◄── Stopping ◄── Recording
                    ▲            ▲
                    │            │ Stop
                    └────────────┘

 Any active state ── unrecoverable error/overflow ──► Error
```

`Triggered` is an observable event between Armed and Recording even if both occur in one UI
frame. A shared `CaptureSessionStatus` snapshot contains session ID, source node ID, state,
committed sample count, trigger sample, recording origin, graph revision, overflow state, and an
optional structured error. It also exposes raw input throughput, staging occupancy, staging write
rate, free storage, raw committed duration, summary-covered duration, graph-processed duration,
decoder lag, and compression ratio. Unknown metrics remain absent rather than being reported as
zero.

The lifecycle state is accompanied by a more precise acquisition phase: WaitingForTrigger,
CapturingOnDevice, ReceivingLiveData, UploadingBufferedData, DrainingPipeline, or Finalizing. This
distinction matters because Recording does not imply that bytes are already available to the host.
Progress contains independently optional captured samples and transferred bytes, so a source never
fabricates capture progress that its hardware cannot report.

Start performs these operations in order:

1. Synchronize all inline node controls and snapshot the graph revision.
2. Resolve exactly one live-capture descriptor and validate the complete graph.
3. Create fresh raw and derived stores and a gated analysis cursor whose origin is not yet fixed.
4. Ask the source feature to open the analyzer, negotiate an immutable effective plan, and configure
   it against the raw-store writer, including its trigger program.
5. Materialize the fixed graph with a live analysis-cursor override and every downstream
   subscription ready.
6. Start/arm the prepared acquisition and enter Armed or immediate Recording. A `Triggered` event
   fixes the recording origin and releases the analysis cursor; free-running capture uses its first
   committed sample.
7. Follow session status and committed extents without blocking the UI thread.

Stop first requests device stop, continues servicing transport completion/drain requirements, finalizes
the raw store, closes the analysis cursor at the final committed sample, and lets the graph drain.
Only then does the UI enter Complete. “Stopping…” therefore means data is still being safely
drained, not that the application ignored the button.

Stop, Abort, and Force Trigger are distinct operations:

- **Stop** requests an orderly device stop, drains all committed data, and finalizes it. Stopping
  while Armed produces a clean `CancelledBeforeTrigger` outcome rather than inventing a trigger.
- **Abort** is reserved for an immediate escape when orderly stop cannot finish. It retains any
  valid committed prefix as an explicitly incomplete session and never labels it Complete.
- **Force Trigger** asks a capable source to use the current sample as the trigger point. It is
  available only while Armed and only when advertised by source capabilities.

Capture Now is a fourth, pre-start action rather than a synonym for Force Trigger. It creates one
session with an immediate recording start while leaving the saved trigger program untouched. This
is useful for inspecting current signals when a complex trigger does not fire. A later ordinary
Start uses the saved trigger program again; the one-shot override never edits graph state.

A trigger timeout is part of capture policy. On timeout, the configured action is to continue
waiting, stop cleanly, or request Force Trigger when supported. The default is to continue waiting.
All three commands are generic capability operations; the UI does not emulate a hardware trigger
by guessing a timestamp.

Closing the application, deleting the source through a future editing mode, source disconnect, and
pipeline failure all use the same stop/finalize path. A forced abort is separately labeled and
never presents a partial file as a clean capture.

For a device-buffered run, orderly Stop requests partial upload when the negotiated plan supports
it, then finalizes the returned prefix. If partial upload is unsupported, the UI explains before
arming that Stop can only cancel without data. Abort always chooses the immediate discard path.
For a host-streamed run, Stop drains the already transferred prefix in the normal way.

### Driver event contract

The portable analyzer boundary exposes acquisition events rather than hiding trigger information
inside `next_chunk`:

```rust
pub enum LogicCaptureEvent {
    Armed,
    Triggered { sample: u64 },
    PhaseChanged(CapturePhase),
    Progress(CaptureProgress),
    Data(CanonicalDigitalChunk),
    Overflow,
    Finished,
}
```

For U3Pro16, the 1024-byte trigger header produces `Triggered` using its trigger-position field.
The current remaining-count validation remains in the driver. No-trigger capture publishes an
immediate trigger at the first recorded sample. Data chunks preserve monotonic sequence and sample
ranges; the driver must not discard or reinterpret an unaligned narrow-mode tail.

The U3 protocol places the trigger header before its data stream. Consequently the UI can show
Armed status immediately, but cannot claim to display pre-trigger samples before the device
delivers them. Once delivery begins, every returned chunk is uploaded. A later analyzer capable of
continuous pre-trigger delivery can use the same event/store contract.

Hardware run-length encoding is preserved as part of the negotiated plan and raw session metadata.
The concrete provider converts device bytes into a versioned canonical packed-sample or canonical
run representation before commit. If the device payload already matches a canonical encoding, its
buffer can be adopted without copying. Optional original-device packets may be retained as a
provenance attachment, but they are never the only replayable copy. Reported progress distinguishes
transport bytes from logical samples. Final replay therefore depends on neither the current graph
setting nor the original hardware driver.

Canonical chunks carry their explicit `CaptureChannelId` table, logical sample range, initial
levels, and either arbitrary-width packed samples or transition runs. They do not assume a maximum
channel count, contiguous hardware numbering, byte-aligned device transfers, or a particular
interleave order. The concrete provider performs those mappings before publication.

### Authoritative live capture store

The uploaded raw stream is the authority for both display and analysis:

```text
 Prepared acquisition events
          │
          ▼
  append shared raw chunk ─────► sequential native staging file
          │                              │
          │                              ├─ incremental waveform summaries
          │                              ├─ viewer viewport queries
          │                              └─ finalized capture / future export
          ▼
  advance committed cursor
          │
          └────────► graph analysis cursor ─► demux ─► decoders/sinks/viewer lanes
```

The same append path accepts chunks transferred continuously during host streaming and chunks
uploaded after device-buffered sampling. In the buffered case the viewer shows phase/progress but
does not draw samples before the first `Data` event. Once upload begins, waveform summaries and the
graph advance incrementally instead of waiting for the complete device buffer.

The device reader never waits for decoder or renderer work. It waits only for the mandatory
sequential store append. Analysis can fall behind and catch up from the committed staging file.
Raw chunk bytes are shared/adopted rather than copied before the append. If the staging device
cannot sustain acquisition or the hardware reports overflow, capture stops with an explicit
loss-of-integrity error; data is never silently dropped.

Before the trigger, committed raw chunks remain available to the session store but the gated graph
cursor emits nothing. On `Triggered`, the store records the exact origin and the cursor begins at
that sample. Thus hardware capture/upload can be active while Armed, whereas graph logging and
derived output begin only when the trigger activates. A later pre-trigger-display option can query
the retained raw prefix without changing analysis semantics.

The native store consists of:

- an append-only interleaved raw-chunk file;
- a small append-only commit log containing sequence, sample range, file offset, and length;
- metadata with physical-channel mapping, sample rate, trigger position, and recording origin;
- an incremental per-channel waveform summary built from committed chunks; and
- a finalized manifest that turns the temporary store into an immutable captured session.

The store initially uses an application cache/session directory, not the graph directory. A later
Save Capture operation can atomically export or move it. Temporary sessions have explicit cleanup
and pinning rules so an open viewer or replay cursor cannot be deleted.

Waveform summaries may lag raw commit, but their covered extent is explicit. The viewer shows raw
session progress and never invents waveform data beyond the summary's committed extent. Summary
building is independent of graph decoding and can use background workers.

### Memory and throughput posture

High-rate capture makes bounded resource use an architectural requirement, not a later
micro-optimization. At 500 MS/s, the ideal bit-packed payload alone is 375 MB/s for six enabled
channels, 500 MB/s for eight, and 1 GB/s for sixteen. A queue sized in hundreds of megabytes
therefore represents less than a second of buffering at some valid settings. Capture duration must
not determine resident memory usage.

The first implementation consequently establishes these invariants:

- acquisition uses a fixed-size pool of reusable, relatively large chunks and bounded queues;
- one immutable canonical chunk is shared by the staging writer, incremental index/summary
  builder, and any consumer that is caught up, rather than copied into separate subsystem queues;
- the staging file remains authoritative once a chunk is committed; a lagging graph or viewer
  releases hot chunks and catches up from the committed store instead of retaining acquisition
  memory;
- a provider adopts a received buffer directly when it already has a canonical encoding, or
  performs at most one canonical transformation before publication;
- the hot path keeps samples bit-packed or run-encoded and does not allocate per-sample objects or
  eagerly demultiplex the entire capture into per-channel arrays; and
- queue occupancy, writer throughput, summary lag, graph lag, and raw input rate remain observable
  so a bottleneck produces a useful warning or integrity error.

The initial vertical slice remains replayable and retains raw data according to `RetentionPolicy`.
It does not require an optimal compression format, a zero-copy path for every provider, or a fully
parallel decoder scheduler before basic live capture works. Those are optimized only after
end-to-end profiles identify the limiting stage. The canonical chunk/store contracts nevertheless
permit later packed layouts, transition indexes, hardware run representations, direct-I/O-sized
writes, and parallel summary construction without changing the coordinator, viewer, or graph
contracts.

A later explicit monitor-only policy may discard raw chunks after all required online consumers
have processed them. That policy cannot provide full Run replay, capture export, or recovery for
discarded ranges, and decoder lag must be reported as data loss rather than hidden. It is therefore
not a transparent optimization and is outside the first vertical slice. Lossless full retention is
the default; bounded rolling retention remains an explicit user choice.

### Common capture-query boundary

The existing finite `CaptureDataSource`/`CaptureIndex` contract evolves into a platform-neutral
capture timeline query whose snapshot can grow:

```rust
pub struct CaptureSnapshot {
    pub metadata: CaptureMetadata,
    pub generation: u64,
    pub committed_samples: u64,
    pub finalized: bool,
    pub trigger_sample: Option<u64>,
}

pub trait CaptureTimeline: Send + Sync {
    fn snapshot(&self) -> CaptureSnapshot;
    fn sampled_window(&self, request: CaptureWindowRequest)
        -> Result<CaptureSampledWindow>;
}
```

File-backed indexes and the live store both implement this query. Filesystem paths and mmap details
stay inside native implementations; the viewer sees only a query handle and generation changes.
This removes the application's current file-source/demo-source branching and keeps the widget
independent of U3Pro16, DSL, and third-party capture formats.

New generations request repaint and extend the scrollbar/fit range. Trigger position renders as a
ruler marker distinct from ordinary cursors.

### Live-view navigation

View navigation is independent of acquisition and analysis. Each viewer panel has one local mode:

- **Follow newest** keeps the latest committed sample at the right edge;
- **Fit growing capture** continually fits the retained extent;
- **Fixed viewport** leaves pan and zoom unchanged while data arrives.

Manual pan or zoom changes an automatic mode to Fixed viewport. A visible Go Live action returns
to Follow newest, and Jump to Trigger centers the exact trigger marker without changing capture
state. An optional preference may jump to the trigger automatically when it arrives.

Pause Display freezes waveform and derived-lane repaint at one generation while acquisition,
staging, summaries, and graph processing continue normally. Resuming jumps to the newest available
generation according to the selected navigation mode. Pause Display is deliberately not another
name for Stop and must never apply backpressure to acquisition.

### Viewer trigger controls

Trigger controls are optional row-label decorations supplied through a generic model:

```rust
pub struct RowTriggerControl {
    pub row: usize,
    pub channel_id: CaptureChannelId,
    pub condition: TriggerCondition,
    pub supported_conditions: Vec<TriggerCondition>,
    pub enabled: bool,
}

pub struct TriggerEdit {
    pub channel_id: CaptureChannelId,
    pub condition: TriggerCondition,
}
```

The viewer paints and hit-tests the icons and exposes `take_trigger_edit`; it does not mutate graph
state or call a driver. Trigger controls appear only on raw channels belonging to the active live
source, never on derived lanes. Disabled inputs and channels without a simple-trigger capability
are absent rather than drawn as usable trigger controls. Different channels may advertise different
condition sets.

The label layout reserves a stable icon column, so names and channel badges do not jump when one
condition changes. Icons remain readable at display scaling and have text tooltips for color- and
shape-independent meaning.

### Logic Analyzer title-bar control

The Logic Analyzer panel title bar receives one combined stateful control:

| Session state | Control | Action |
| --- | --- | --- |
| Idle, Complete, Error | Start icon | Validate graph and begin a new session. |
| Preparing, Armed, Recording | Stop icon | Request orderly stop and drain. |
| Stopping | Disabled stop/progress icon | Wait for hardware, store, and graph drain. |
| No live source / multiple sources | Disabled Start | Tooltip explains the capability error. |

Status beside it shows at least Armed, Triggered/Recording, received duration, and an overflow or
error indicator. A compact health popover exposes buffer occupancy, input and write rates, free
storage, retained duration, summary lag, and graph/decoder lag. Warnings appear before a hard
limit is reached. The existing Node Graph Run control remains separate:

- **Capture Start/Stop** controls hardware acquisition plus its live analysis.
- **Run** re-analyzes the current finalized capture with the current graph.

Run is disabled while capture is active. Start is disabled while a replay run is active. If a live
source has no finalized session, Run explains that the user must capture first instead of opening
the hardware implicitly. While Armed, Force Trigger appears as a capability-gated secondary action;
Abort remains available from the capture control's context menu and from the configured binding.
Capture Now is available in the capture control's secondary menu as a one-shot action when a
trigger program is configured. During device-buffered capture, status distinguishes capture
progress from upload progress; during host streaming, it reports the growing committed duration.

### Fixed graph and immutable run boundary

Phase one switches `NodeGraphWidget` into generic read-only mode at Preparing and restores editing
only after Complete or Error cleanup. Selection, inspection, pan, zoom, and copy remain available;
mutating actions and inline controls are disabled. This makes the graph revision shown in session
metadata exact and prevents ambiguous half-applied hardware settings.

Trigger icons are also disabled once Preparing starts because they affect acquisition. This is
different from viewer pan/zoom and cursors, which remain interactive.

### Re-analysis with Run

The finalized session is associated with the live source's `NodeId` in application document state.
Both live analysis and Run supply the same generic source override to lowering:

```rust
pub enum CaptureInputOverride {
    FollowLive(LiveCaptureCursor),
    Replay(CapturedSession),
}

CompileCtx::capture_overrides: HashMap<NodeId, CaptureInputOverride>
```

The U3Pro16 builder consumes either override and builds a capture-cursor source instead of opening
USB. Generic lowering only matches the explicit node ID and capability; it never checks the node
name. The live and replay cursors expose the same enabled physical-channel mapping and sample
timestamps as the hardware source, so all downstream nodes are unchanged. Run selects `Replay`
and creates fresh derived stores; capture Start selects `FollowLive` before arming the separately
prepared hardware acquisition.

Re-analysis always starts from the recording origin and processes the immutable capture with the
current graph. It uses a fresh `DerivedLanes` generation, so old live-derived results are replaced
atomically rather than appended to or patched. Re-analysis never changes raw capture data.

### Future configuration epochs

After fixed-graph capture is reliable, ordinary processor parameters may change while Recording.
Every accepted change receives an effective sample boundary and monotonically increasing epoch ID.
Nodes apply the change only to subsequent input. Already emitted words, markers, files, and viewer
lanes remain untouched.

The existing live manager supplies add/remove/hot-reconfigure/restart mechanics. The capture
session adds the missing durable facts:

- graph revision or patch;
- epoch ID;
- effective sample/timestamp; and
- success/failure outcome.

Acquisition settings that hardware cannot change safely—sample rate, channel mask, simple trigger,
clock source, and encoding—remain deferred to the next capture session. A future driver capability
may explicitly permit a subset at safe boundaries.

Re-analysis normally ignores live epochs and runs the current graph from the start. Reproducing the
original live analysis from its epoch log is a separate explicit mode.

### Session ownership, replacement, and recovery

Starting a capture does not destroy the current completed session. The previous session remains
viewable throughout Preparing and until the new session has its first valid data commit. The viewer
then switches to the new session, and the prior session moves into a bounded recent-captures list.
A failed preparation therefore leaves the previous display and Run input unchanged.

Each session has a durable identity and one explicit outcome: Complete, CancelledBeforeTrigger,
Incomplete, or Corrupt. Clean completion does not imply that the session has been saved to a user
location. The application shows an unsaved marker and asks before evicting the last reference to an
unsaved session. The recent list supports reopening a session, making it the Run input, saving it,
or explicitly discarding it. Its count and storage budget are configurable.

The append-only commit log and manifest are recovery records as well as live indexes. Metadata and
commit records are flushed at bounded intervals, with the interval chosen so it does not stall the
device reader. On startup, the application scans only its session directory and offers recoverable
incomplete sessions. Recovery validates sequence ranges and checksums, truncates an uncommitted
file tail, preserves the valid committed prefix, and keeps the Incomplete outcome visible. A
session is never auto-deleted before this recovery decision merely because the previous process
did not shut down cleanly.

### Later live-capture capabilities

The contracts reserve capability operations for these later additions without requiring them in
the first vertical slice:

- repeated or segmented acquisition that re-arms after each trigger and records immutable frames
  within one session, with a configurable re-arm delay and frame limit;
- advanced trigger stages including pulse width, holdoff, debounce/glitch qualification, channel
  patterns, and conditions derived from decoded events. Capability metadata describes maximum
  sequential stages, condition planes, logical operations, equality/inversion, event counters,
  contiguous-count qualification, and serial shift-register fields rather than assuming every
  source has the same trigger engine;
- live search and incremental measurements over raw or derived lanes, with explicit covered extent
  and processing lag;
- configurable system notifications for trigger, completion, disconnect, overflow, and low
  storage;
- an automation service for configuring, arming, monitoring, stopping, saving, and exporting
  sessions without driving UI widgets;
- external trigger input/output, shared sample clocks, and timestamp alignment for synchronized
  sources; and
- a host capability that inhibits automatic system sleep during active acquisition and reports
  suspend/resume as an integrity event when inhibition is unavailable.

Repeated acquisition uses `CaptureFrameId` and per-frame trigger/origin metadata from the start;
it does not concatenate frames into a falsely continuous sample range. Search and measurements use
the same committed-prefix query boundary as the viewer. Automation invokes the same coordinator
commands as the UI, so it cannot bypass validation, graph locking, or finalization.

The advanced Triggers panel consumes a generic `TriggerEditorSchema` and emits neutral edit
operations. The schema describes supported predicates, typed operands, stage/sequence structure,
limits, defaults, and validation messages using stable registered IDs. Concrete features lower and
serialize the resulting program. The panel contains no built-in driver layouts, model checks, port
label inference, or arbitrary device callbacks.

### Persistence and future export

The graph file stores capture and trigger *configuration*, not temporary raw bytes. Simple trigger
conditions are part of `U3Pro16State`. Application document metadata may record the current capture
reference, source node ID, and session manifest once the capture has a durable location.

Each session manifest stores both the requested policy and negotiated effective plan, acquisition
profile, device/firmware identifiers needed to interpret the stream, internal or external timebase,
logical sample count, encoded byte count, trigger placement and actual trigger sample, phase/outcome
history, and frame metadata. Replay uses these facts rather than current node defaults.

The finalized internal session is the lossless source for exporters:

- a DSL exporter writes raw physical channels, names, sample rate, and trigger position;
- portable capture exporters write the equivalent session to supported interchange formats; and
- derived-data export uses explicit format capabilities rather than silently dropping unsupported
  lane types.

Saving raw capture and saving derived analysis are separate checkable operations even when one
dialog offers both. The application warns when a target format cannot represent a derived payload.
Exporters live in `logic_analyzer_processing`; file dialogs and overwrite confirmation remain in
the native application service.

### Failure and integrity rules

- Device/link overflow, sequence gaps, short writes, and staging-write failures are fatal integrity
  errors.
- No component substitutes a partial session for a clean Complete session.
- Device stop is idempotent; repeated Stop requests do not issue conflicting control transfers.
- Trigger wait is cancellable without waiting for a trigger to occur.
- Force Trigger is issued only through an advertised source capability and records the device's
  acknowledged sample position.
- The UI thread performs no device/link, staging-file, summary-build, or capture-query I/O.
- A slow graph can lag acquisition because it reads the store independently; lag is visible.
- Pausing display updates does not pause acquisition or consume additional unbounded queue space.
- Low-storage and buffer-pressure warnings are raised before exhaustion; exhaustion still produces
  an explicit integrity outcome rather than silently shortening retention.
- Store cleanup cannot remove a session pinned by the viewer, an analysis cursor, or an exporter.
- Hardware and graph errors retain the successfully committed raw prefix for explicit recovery or
  discard, with its incomplete status visible.
- Abrupt termination leaves a recoverable commit-log prefix; recovery never guesses that an
  uncommitted tail is valid.

### Delivery plan

The phases below are dependency ordered and intentionally produce runnable vertical increments.
Each phase has one principal risk and a completion gate. A phase may use several pull requests, but
work on the next phase starts only after its gate passes. Every gate includes focused tests,
`cargo test --workspace`, and
`cargo check -p logic-analyzer-app-web --target wasm32-unknown-unknown`. Native-only implementations
remain behind whole-module platform boundaries so the wasm check does not spread conditional code.

The existing `LogicAnalyzerSource` graph-run path remains operational while Phases 1–8 establish
and prove the parallel session path. The fake source uses live analysis in Phase 5; the concrete
U3Pro16 graph path switches only in Phases 9–10. Early phases therefore do not leave ordinary graph
runs half-migrated. Test providers are registered only by test/development composition and never by
matching their names in application or generic code.

#### Phase 1 — Provider lifecycle and deterministic fake

- Add provider-neutral session identity, lifecycle state, phase/progress, structured errors,
  canonical raw-chunk ownership, and the bounded writer/event boundary.
- Add the object-safe prepared-acquisition lifecycle in `logic_analyzer_processing`: Prepare,
  Start, idempotent Stop, event publication, and Join.
- Add one deterministic continuously delivering fake provider as reusable test support. It emits a
  known packed pattern in deliberately awkward chunk boundaries and can be stopped at a known
  sample.
- Keep graph discovery, UI, persistence, triggers, summaries, U3Pro16, and replay out of this phase.

Gate: a headless test observes the exact Prepare/Start/Data/Stop/Complete event order, reconstructs
the fake samples byte-for-byte, proves repeated Stop is harmless, and proves outstanding chunks and
queue occupancy never exceed configured bounds.

The first code changes belong in a new generic live-capture module in `signal_processing` and the
portable analyzer lifecycle in `logic_analyzer_processing`. `logic_analyzer_graph`,
`logic_analyzer_ui`, and `logic_analyzer_viewer` remain unchanged in this phase. This is the defined
starting point for implementation.

#### Phase 2 — Minimal authoritative store

- Implement sequential native raw staging, the smallest durable commit log, a committed-prefix
  cursor, finalization, and a reader for finalized sessions.
- Use the bounded reusable chunk pool and share/adopt canonical chunks rather than creating a
  second acquisition-sized queue.
- Defer incremental waveform summaries, retention reclamation, crash recovery, cleanup policy, and
  export.

Gate: fake-provider input is committed and replayed byte-for-byte across unaligned chunk and sample
boundaries; a deliberately paused reader does not block acquisition; resident memory reaches a
fixed bound during a long synthetic capture.

#### Phase 3 — Immediate-capture application integration

- Add the optional generic `LiveCaptureFeature` discovery contract to `RuntimeBuilder` and expose
  the fake provider through test/development registration.
- Add the application capture coordinator, title-bar Start/Stop, basic status, orderly drain, and
  graph read-only state while capture is active.
- Support immediate capture only. Do not add trigger controls, policies, waveform display, or graph
  processing yet.

Gate: an application integration test starts and stops the fake source through the same commands as
the title bar, displays every lifecycle state, restores graph editing after drain, and produces a
finalized session.

#### Phase 4 — Growing live waveform

- Evolve the capture query into a growing timeline and build incremental waveform summaries from
  committed chunks.
- Connect the viewer to placeholder/live physical lanes and add Follow Newest, Pause Display, and
  Go Live behavior.
- Keep graph analysis independent and disabled for this phase.

Gate: the fake waveform becomes visible before capture completes, paused display does not delay
acquisition, Go Live catches up, and the finalized waveform matches the fake input at exact and
summary zoom levels.

#### Phase 5 — Independent live graph analysis

- Add the independent analysis cursor and feed the fixed compiled graph from committed raw chunks.
- Start at the immediate recording origin, expose graph lag, and let a lagging graph catch up from
  the store rather than retaining hot acquisition chunks.
- Do not add post-capture Run replay or triggers yet.

Gate: a deliberately throttled decoder falls behind without slowing acquisition, subsequently
catches up without a sequence gap, and produces the same derived output as processing the same
finite fake input.

#### Phase 6 — Finalized-session Run replay

- Add node-ID source overrides and make Run read the finalized raw session without opening a live
  provider.
- Recreate derived stores for each replay and preserve the captured channel/timebase metadata.

Gate: live-derived and replay-derived outputs for a finalized fake session are byte-for-byte equal,
and an instrumented provider proves that replay performs no discovery, open, or device operation.

#### Phase 7 — Portable simple triggering

- Add the common Ignore/Low/High/Rising/Falling/Either trigger model, neutral feature edits,
  per-lane icons, Armed/Triggered status, and recording-origin gating.
- Persist the requested trigger in the test/development feature and establish the explicit
  migration/diagnostic contract, but lower and exercise it against a trigger-capable fake provider
  before using real hardware. Concrete U3Pro16 state migration remains in Phase 9.
- Exclude advanced stages, serial triggers, trigger placement, timeout actions, and Force Trigger.

Gate: every simple condition and disabled-channel case has a deterministic trigger sample; the
viewer marks it; graph output begins at the defined recording origin; save/load and migration tests
preserve the requested trigger with user-visible compatibility diagnostics.

#### Phase 8 — Provider-neutrality conformance

- Add the second deliberately different fake provider required by the architecture: it buffers on
  the device, exposes data only during upload, lacks Force Trigger, and advertises a different
  setting matrix and non-contiguous, bank-qualified channel identifiers.
- Run both providers through the same lifecycle, store, coordinator, viewer, graph, replay, and
  trigger contract suites without provider-specific branches in generic code.
- Keep real hardware and new product behavior out of this phase; it exists to challenge the
  contracts before they become expensive to change.

Gate: both fake providers pass the shared conformance suite, registration requires no generic
source edits, and architecture tests find no provider/model-name branches in the application,
compiler core, viewer, session coordinator, or store.

#### Phase 9 — U3Pro16 device-buffered acquisition

- Register the concrete U3Pro16 live feature, evolve its saved state explicitly, and lower generic
  channel, rate, depth, simple-trigger, and timebase requests into its provider representation.
- Negotiate an immutable device-buffered plan, publish the actual trigger position, upload all
  returned chunks, and preserve hardware run encoding through the canonical-store boundary.
- Keep host streaming and the broader capture-policy UI out of this phase.

Gate: packet-fixture tests cover configuration and trigger-header translation; an ignored hardware
test completes one buffered capture and replay; generic crates contain no U3/model/port-name
branches.

#### Phase 10 — U3Pro16 host streaming and sustained ingest

- Add the separate host-streamed acquisition profile, its channel/rate/link matrix, live delivery,
  stop behavior, and explicit overflow/integrity handling.
- Benchmark acquisition, canonicalization, staging writes, summary work, graph lag, and resident
  memory at representative channel/rate classes.
- Optimize only a measured limiting stage; do not make a speculative codec or scheduler a phase
  prerequisite.

Gate: long captures have duration-independent resident memory, a slow optional consumer cannot
block the device reader, unsupported rate tuples are rejected, and every loss/overflow condition is
reported rather than silently discarded.

#### Phase 11 — Capture policies and health controls

- Add finite completion, rolling retention, trigger placement, timeout actions, Capture Now, Force
  Trigger, Abort, capacity estimates, and health/lag telemetry through advertised capabilities.
- Persist requested policy in the concrete graph state and negotiated effective values in the
  captured session.

Gate: the deterministic providers cover every supported policy composition and rejection path;
pinning and reclamation never remove required data; UI commands never imply an unsupported device
operation.

#### Phase 12 — Recovery and session ownership

- Add recovery after every durable commit step, incomplete-session presentation, cleanup and
  pinning, recent-session ownership, and explicit keep/discard decisions.
- Keep export out of this phase so lifecycle and deletion safety are verified independently.

Gate: fault-injection tests recover exactly the committed prefix or return a structured corruption
error, and no pinned viewer, analysis, or future-export session can be removed.

#### Phase 13 — Export

- Add raw DSL and supported portable interchange export from finalized sessions.
- Add capability-aware derived export only after raw export is reliable.

Gate: exported raw captures reopen with identical channels, sample rate/timebase, samples, and
trigger position; unsupported derived values produce an explicit warning rather than omission.

#### Phase 14 — Extended live workflows

- Add configuration epochs and graph changes that affect only future samples.
- Add advanced staged/counted/serial triggers, repeated or segmented acquisition, live search and
  measurements, notifications, automation, source synchronization, and power-management
  integration as separately scoped follow-up work.

Gate: each follow-up receives its own focused design amendment and acceptance gate before
implementation; Phase 14 is not a single release-blocking batch.

### Verification strategy

Most tests use deterministic fake providers; USB hardware is not required for correctness
coverage.

At least two deliberately different fake providers are mandatory. One uses more than sixteen
bank-qualified, non-contiguous channel IDs and a continuously available non-USB transport. The
other buffers on-device, exposes data only during upload, lacks Force Trigger, and supports a
different setting matrix. Both must pass the same coordinator/store/viewer suite without generic
source changes. This is the architectural proof that the first hardware implementation did not
become the contract.

- Trigger lowering tests cover every condition, physical/logical channel mapping, disabled inputs,
  no-trigger free run, trigger placement/alignment, one-shot trigger bypass without saved-state
  mutation, and old saved graphs.
- Session state tests cover Start, trigger, Stop-before-trigger, orderly drain, repeated Stop,
  Capture Now, Force Trigger, Abort, timeout actions, buffered partial upload/discard, disconnect,
  overflow, and staging failures.
- Store contract tests compare live queries and finalized replay against the original packed input,
  including unaligned 3/6/12-channel chunk boundaries and block boundaries. Retention tests cover
  everything, recent-duration, recent-byte, pinning, reclamation, and recovery after every commit
  boundary.
- Concurrency tests pause graph analysis while acquisition continues, then verify exact catch-up
  without a sequence gap.
- Viewer tests cover icon layout, scaling, hit testing, tooltips, row reorder/rename interaction,
  absence on derived/file lanes, navigation-mode transitions, Pause Display isolation, Go Live,
  and Jump to Trigger.
- Capacity and health tests cover worst-case input estimates, low storage, buffer pressure,
  independently lagging summaries/graph processing, and absent metrics.
- Plan-negotiation tests cover every supported channel/rate/transport tuple, link-speed changes,
  hardware-depth limits, encoding restrictions, fixed versus selectable trigger placement, unknown
  external-clock rates, and rejection without silent clamping.
- Compiler tests reject multiple live sources, preserve node-ID mapping, and prove replay overrides
  never open hardware.
- Registration tests add the second fake provider through the public feature registry and verify
  that application, compiler-core, viewer, and store source files contain no driver/model-name
  branches.
- Golden tests compare live-derived and replay-derived outputs byte-for-byte for one finalized mock
  session.
- Native integration tests exercise U3Pro16 packet/header translation behind an ignored hardware
  test, including distinct buffered and streamed event orders, progress, actual trigger position,
  partial upload, and encoded logical-sample counts. Native and wasm compilation verifies complete
  platform-module exclusion without scattered target conditionals.
- Throughput tests report USB ingest, staging throughput, summary lag, graph lag, and resident
  memory at each supported channel-width/rate class. Long-duration tests verify that resident
  memory reaches a bound independent of capture duration, and instrumentation verifies that each
  input chunk undergoes no more than one canonical transformation.
- Recovery tests terminate after each durable write step and prove the application either restores
  exactly the committed prefix or reports a structured corruption error.
