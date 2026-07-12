# WASM Storage Platform Design

## Goal

Keep decoded-word storage, viewer lanes, and compiler code platform-neutral.
Platform conditionals belong at implementation-file boundaries. Native builds
use file-backed, mmap-backed persistent storage; wasm builds use an in-memory
implementation with the same query and writer contracts.

File I/O, mmap, filesystem cache discovery/cleanup, USB access, and other
genuinely unavailable facilities may remain unavailable on wasm. Their entire
implementation files should be selected or excluded, rather than placing
`#[cfg(target_arch = "wasm32")]` on fields, enum variants, match arms, imports,
and statements throughout consumers.

## Current problem

The native derived-word store was introduced as a platform-specific type. Its
platform choice consequently leaked through:

- `ViewerSink` lane variants, fields, constructors, work loops, and tests;
- logic-analyzer channel, cursor, drawing, sampling, and viewer code;
- compiler context, compiled graph state, cache inventory, live-run state, and
  viewer builder code;
- public re-exports in `dsl` and UI crates.

This makes wasm and native compile different application models. It also makes
ordinary refactoring require paired `cfg` edits across unrelated crates.

## Stable platform-neutral surface

The following concepts must exist on every target:

- `AnnotationQuery` and its metadata/window/result types;
- `IndexedAnnotationStore` (read/query handle);
- `IndexedAnnotationWriter` (append/finish/cancel handle);
- `LiveStoreConfig`;
- `StoreStatus` and a platform-neutral `StoreError`;
- `IndexedAnnotationLane` and `DerivedLaneData::IndexedAnnotations`;
- cache/persistence configuration as an optional capability, not a different
  graph or lane shape.

The name `IndexedAnnotationStore` describes its behavior, not its physical
medium. On native it may be disk/mmap backed; on wasm its index and exact words
live in memory.

## Backend contracts

Use small private traits behind the stable facade:

```rust
trait AnnotationStoreBackend: AnnotationQuery + Send + Sync {
    fn snapshot(&self) -> LiveStoreSnapshot;
    fn append(&self, words: &[Word]) -> StoreResult<()>;
    fn finish(&self) -> StoreResult<()>;
    fn cancel(&self);
}

trait PersistentCacheBackend: Send + Sync {
    fn open(&self, config: &PersistentStoreConfig)
        -> StoreResult<Option<IndexedAnnotationStore>>;
    fn clear_entry(&self, config: &PersistentStoreConfig) -> StoreResult<()>;
    fn cleanup(&self, directory: &Path, max_bytes: u64) -> StoreResult<()>;
}
```

The exact ownership can use an `Arc<dyn AnnotationStoreBackend>` shared by the
store and writer handles. The public facade delegates and contains no target
conditionals.

The persistent-cache trait is deliberately separate. In-memory annotation
storage is useful on wasm; filesystem persistence is not. The wasm cache
implementation returns `Ok(None)` for open and a typed `Unsupported` result
for explicitly requested filesystem operations. Normal viewer operation must
not call those unsupported operations.

## File layout and platform selection

Common code:

```text
derived_word_store/
  mod.rs                 public facade and common types
  backend.rs             private backend traits
  codec.rs               platform-neutral encoding
  format.rs              platform-neutral format definitions
  presence.rs            platform-neutral presence index
  query.rs               platform-neutral query contract
  platform/
    mod.rs               the only target selection point
    native.rs            files, mmap, persistent cache, decoded-block cache
    wasm.rs              in-memory blocks and presence index
```

Selection is at the whole-file boundary:

```rust
#[cfg_attr(target_arch = "wasm32", path = "wasm.rs")]
#[cfg_attr(not(target_arch = "wasm32"), path = "native.rs")]
mod imp;

pub(crate) use imp::PlatformBackend;
```

If `cfg_attr(path = ...)` proves awkward for tooling, two module declarations
inside `platform/mod.rs` are acceptable. Target conditionals must not appear in
callers.

Native-only cache administration can use the same pattern in a separate
`cache_platform` module. This keeps unavoidable filesystem switches at a
module boundary.

## Viewer and sink design

`DerivedLaneData::IndexedAnnotations` and `IndexedAnnotationLane` exist on all
targets. The lane holds an `Arc<dyn AnnotationQuery>` and platform-neutral
status/metadata handles. Drawing, cursor snapping, sampling, and row handling
therefore use one code path.

`ViewerSink` always owns the same optional writer/query fields. Store creation
selects the backend internally. Wasm receives a bounded or unbounded in-memory
store based on `ViewerRetention`; native may additionally persist it.

The existing plain `Annotations(Vec<Annotation>)` lane remains useful for
small, explicitly non-indexed streams. It must not be the platform fallback
for the same compiled graph, because that recreates platform-specific lane
shapes.

## Compiler design

Compiler IR and live-run state must not contain target-gated fields or enum
variants. The compiler always describes the requested storage behavior:

- exact-history retention;
- optional persistent cache key/directory;
- memory/entry budget;
- indexing requirement.

Backend capability resolution happens when the viewer sink/store is built.
On wasm, persistence requests become a warning/capability result while the
in-memory indexed lane still runs. Nodes that fundamentally require native
resources—file readers, file writers, USB devices—remain excluded as complete
node modules and registry entries.

## Unavoidable target gates

These switches are acceptable when applied to whole files/modules or registry
entries:

- file and directory I/O;
- mmap and advisory locking;
- persistent cache cleanup/discovery;
- native worker pools or threads unavailable in the wasm runtime;
- USB/device capture backends;
- native file dialogs and native application integration.

OS-family switches inside a native implementation (`unix`, `windows`, macOS
filesystem behavior) are also legitimate.

## Migration plan

1. Introduce the backend traits and platform-neutral store facade without
   changing native behavior.
2. Move the current file/mmap/cache implementation into `platform/native.rs`.
3. Implement `platform/wasm.rs` using the codec, presence index, and an
   in-memory ordered block store.
4. Make all store/query/config/status types available on every target.
5. Remove target gates from `DerivedLaneData`, `IndexedAnnotationLane`, and
   `ViewerSink`; use the common facade.
6. Remove target gates from logic-analyzer channel, cursor, drawing, sampling,
   and viewer code.
7. Remove storage-related target gates from compiler IR, compiler context,
   live-run state, and viewer builder code.
8. Keep native-only source/sink nodes switched at whole module and registry
   boundaries.
9. Add native and wasm contract tests for identical append, exact-window,
   presence-window, nearest-boundary, finish, and cancellation behavior.
10. Add a CI check that builds/tests native and runs `cargo check` for
    `wasm32-unknown-unknown`.

## Acceptance criteria

- No `target_arch = "wasm32"` conditionals in `viewer_sink.rs` for derived-word
  lane shape or store operation.
- No storage-related wasm conditionals in logic-analyzer drawing, cursor,
  channel, sampling, or viewer modules.
- No storage-related wasm conditionals in generic compiler IR/live-run code.
- `derived_word_store/mod.rs` contains no per-item target-gated public API.
- Target selection is confined to platform module files and genuinely
  unavailable native node registrations.
- Native persistent cache behavior and current tests remain unchanged.
- Wasm supports indexed in-memory annotation queries with the same semantics.

