# Decoder View Lane Design

## Architecture boundary

Generic layers do not contain decoder-specific behavior.

`node_graph`, `logic_analyzer_viewer`, and generic graph compiler infrastructure remain
independent of UART, SPI, Binary Decoder, and all other concrete node types. Protocol behavior
belongs in:

- the node definition in `crates/logic_analyzer_graph/src/nodes/`;
- its UI/runtime builder in `crates/logic_analyzer_graph/src/compiler/`;
- its runtime implementation in `crates/signal_processing/src/nodes/`.

The generic viewer renders derived lanes from generic metadata. Saved-graph compatibility is
handled at node restore/load boundaries and reported with user-visible warnings.

## Current lane model

Runtime nodes publish ordinary derived lanes with stable names and presentation metadata. The
viewer renders each lane without knowing which node or protocol produced it.

UART bits and data still use temporary name-based pairing through `uart_data_lane_name` and
`Bits`/`Data` matching. This is legacy node-specific behavior in generic code and is tracked for
removal in [TODO.md](../TODO.md).

## Proposed future lane-group contract

A generic `ViewerLaneGroup` / `ViewerLaneTrack` contract can represent several derived tracks as
one visual lane without exposing protocol concepts to generic code.

A group provides:

- a stable group identifier;
- a display label;
- ordered tracks;
- a relative height for each track;
- optional generic value-presentation metadata.

Each track carries an ordinary derived-lane payload. A UART-specific builder can describe a
bits track and a data track, while the viewer sees only generic tracks, ordering, and geometry.
No grouping is inferred from display names.

Saved graphs can retain legacy sockets during migration, but compatibility remains an explicit
node-level load concern rather than a generic compiler or viewer special case.
