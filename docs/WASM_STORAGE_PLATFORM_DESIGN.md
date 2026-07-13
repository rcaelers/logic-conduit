# WASM Storage Platform Design

## Design

Decoded-word storage, viewer lanes, and compiler code use a platform-neutral data model.
Native builds use file-backed and mmap-backed persistent storage; wasm builds use an in-memory
store with the same query and writer contracts.

Platform selection occurs at implementation-file boundaries. File I/O, mmap, filesystem cache
administration, USB access, native dialogs, and similar unavailable capabilities are selected or
excluded as complete implementations or node registrations. Consumers do not contain target
conditionals for fields, enum variants, match arms, functions, or statements.

## Platform-neutral surface

The following concepts exist on every target:

- `AnnotationQuery` and its metadata, window, and result types;
- `IndexedAnnotationStore` and `IndexedAnnotationWriter`;
- `LiveStoreConfig` and `BlockCodecConfig`;
- `StoreStatus` and platform-neutral store errors;
- `IndexedAnnotationLane` and `DerivedLaneData::IndexedAnnotations`;
- optional persistence configuration without a different graph or lane shape.

`IndexedAnnotationStore` names the behavior of the store rather than its physical medium. Native
stores can be disk- and mmap-backed. Wasm stores keep their index and exact words in memory.

## Backend contracts

Private backend traits sit behind the public facade. The store and writer handles delegate to
these traits, so callers use the same API on every platform.

```rust
trait AnnotationStoreBackend: AnnotationQuery + Send + Sync {
    fn snapshot(&self) -> LiveStoreSnapshot;
}

trait AnnotationStoreWriterBackend: Send + Sync {
    fn append(&self, words: &[Word]) -> StoreResult<()>;
    fn finish(&self) -> StoreResult<()>;
    fn cancel(&self);
}
```

Persistent-cache administration is a separate native capability. In-memory indexed annotation
storage is available on wasm, while filesystem persistence is not part of the wasm contract.

## File layout and platform selection

The derived-word store separates common contracts and data structures from complete platform
implementations:

```text
derived_word_store/
  mod.rs                 facade and common types
  backend.rs             private backend traits
  config.rs              platform-neutral configuration
  presence.rs            platform-neutral presence index
  query.rs               platform-neutral query contract
  state.rs               shared status and metadata
  store.rs               native file-backed implementation
  store_wasm.rs          wasm in-memory implementation
  platform/
    mod.rs               target-selection point
    native.rs            native exports and implementation wiring
    wasm.rs              wasm exports and implementation wiring
```

`platform/mod.rs` selects one complete implementation file:

```rust
#[cfg_attr(target_arch = "wasm32", path = "wasm.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod imp;
```

The native implementation owns codec, file, mmap, persistent-cache, and decoded-block-cache
details. The wasm implementation owns its in-memory representation. Target conditionals do not
propagate into callers.

## Viewer and sink design

`DerivedLaneData::IndexedAnnotations` and `IndexedAnnotationLane` exist on every target. Each
lane holds an `Arc<dyn AnnotationQuery>` plus platform-neutral status and metadata handles.
Drawing, cursor snapping, sampling, and row handling therefore use one code path.

`ViewerSink` uses the same optional writer and query fields on every target. Store construction
selects the backend internally. The plain `Annotations(Vec<Annotation>)` lane remains available
for explicitly non-indexed streams; it is not a platform-specific substitute for an indexed lane.

## Compiler design

Compiler IR and live-run state have the same fields and variants on every platform. The compiler
describes storage requirements such as exact-history retention, persistence settings, cache
budgets, and indexing. Backend construction resolves platform capabilities.

Nodes that fundamentally require native resources, including file readers, file writers, and USB
devices, are selected as complete node modules and registry entries.

## Permitted target gates

Target gates are confined to whole files, modules, or registry entries for:

- file and directory I/O;
- mmap and advisory locking;
- persistent cache cleanup and discovery;
- native worker pools or threads unavailable in the wasm runtime;
- USB and device capture backends;
- native file dialogs and application integration.

OS-family selection inside a native implementation is also valid.

## Invariants

- `viewer_sink.rs` has one derived-word lane shape and one store operation path.
- Logic-analyzer drawing, cursor, channel, sampling, and viewer modules are independent of the
  storage platform.
- Generic compiler IR and live-run code are independent of the storage platform.
- `derived_word_store/mod.rs` does not expose per-item target-gated API variants.
- Native and wasm backends implement identical append, exact-window, presence-window,
  nearest-boundary, finish, and cancellation semantics.
- Native persistent cache behavior is an additional capability, not a different application
  model.
