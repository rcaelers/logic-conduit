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
