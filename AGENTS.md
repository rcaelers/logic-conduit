# Architecture boundaries

- Keep `node_graph`, `logic_analyzer_viewer`, and generic compiler/runtime
  infrastructure independent of concrete nodes and protocols. They must not
  branch on node names, port labels, or protocol-specific values (for example
  UART, `Bits`, `Data`, start/stop markers, SPI, or Binary Decoder).
- Concrete behavior belongs in the corresponding node definition, its
  node-specific UI builder, and its DSL runtime node.
- Pass protocol-specific presentation needs to generic infrastructure through
  explicit, generic metadata/contracts. Do not infer behavior from display
  names or use name-based special cases.
- Preserve saved-graph compatibility through explicit node migration/load
  handling with user-visible warnings; do not hide compatibility work in
  generic viewer/compiler code.

See `docs/DECODER_VIEW_LANE_DESIGN.md` for the detailed viewer-lane decision.

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
migration design and acceptance criteria.
