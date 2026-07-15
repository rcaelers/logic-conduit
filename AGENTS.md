# Architecture boundaries

- Keep `node_graph`, `logic_analyzer_viewer`, and generic compiler/runtime
  infrastructure independent of concrete nodes and protocols. They must not
  branch on node names, port labels, or protocol-specific values (for example
  UART, `Bits`, `Data`, start/stop markers, SPI, or Binary Decoder).
- Concrete behavior belongs in the corresponding `logic_analyzer_graph` node
  feature and its `logic_analyzer_processing` runtime node.
- Pass protocol-specific presentation needs to generic infrastructure through
  explicit, generic metadata/contracts. Do not infer behavior from display
  names or use name-based special cases.
- Preserve saved-graph compatibility through explicit node migration/load
  handling with user-visible warnings; do not hide compatibility work in
  generic viewer/compiler code.

See `docs/DECODER_VIEW_LANE_DESIGN.md` for the detailed viewer-lane decision.

# Crate boundaries

- `signal_processing` is UI-independent generic runtime, capture, and derived-data
  infrastructure.
- `logic_analyzer_processing` owns UI-independent concrete capture sources,
  protocol decoders, processing nodes, and sinks.
- `logic_analyzer_graph` owns concrete graph nodes, compiler builders, graph
  lowering, and plugin registration contracts.
- `logic_analyzer_ui` composes the widgets and application services; it must not
  contain concrete node definitions or runtime builders.
- Reusable widgets live below `crates/widgets` and must remain independent of
  concrete nodes and protocols.

# Platform boundaries

- Do not scatter `#[cfg(target_arch = "wasm32")]` or its inverse across
  fields, enum variants, match arms, functions, or statements in generic
  runtime, compiler, viewer, or node code.
- Represent platform differences behind explicit capability traits with a
  native implementation and a wasm implementation. Consumers must compile
  against one platform-neutral contract and data model.
- When functionality genuinely cannot exist on a target—such as filesystem
  I/O, mmap, USB access, or native dialogs—select or exclude the complete
  implementation file/module or node registry entry. Keep target selection at
  that boundary rather than propagating it into callers.
- Prefer a single `platform` module as the target-selection point. New inline
  wasm conditionals require a documented reason why a trait or whole-file
  implementation boundary is not viable.

See `docs/WASM_STORAGE_PLATFORM_DESIGN.md` for the derived-word-storage
platform design and invariants.

# Design documentation

- Design documents describe the current architecture in present tense.
- Treat unqualified design statements as implemented system behavior; do not
  add implementation-status labels, completed rollout steps, resolved-problem
  sections, or implementation history.
- Put unimplemented ideas only in clearly labeled proposed-future sections and
  track actionable work in `TODO.md`.
- Use version control for historical context instead of preserving it in
  current design documents.

# Rust imports

- Group `use` statements in this order, separated by one blank line:
  language crates (`std`, `core`, `alloc`), third-party crates, other crates
  in this workspace, then the current crate (`crate`, `self`, `super`).
- Run `scripts/sort_use_groups.rb` after adding or reorganizing imports;
  ordinary `cargo fmt` preserves the workspace-specific split.
