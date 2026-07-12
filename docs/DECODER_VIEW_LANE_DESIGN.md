# Decoder View Lane Design

## Decision

The generic layers must not contain decoder-specific behavior.

In particular, `node_graph`, `logic_analyzer_viewer`, and the generic graph
compiler must not know about UART, SPI, Binary Decoder, or any other concrete
node type. They must not inspect node titles, output names such as `Bits` or
`Data`, or protocol-specific values such as UART start/stop/error markers.

Concrete node behavior belongs only in:

- the node definition in `crates/ui/src/nodes/`;
- its runtime builder in `crates/ui/src/compiler/`;
- its runtime implementation in `crates/dsl/src/nodes/`.

The UI compiler may translate a node-specific display declaration into a
generic viewer contract, but the rest of the compiler remains node-agnostic.

## Generic viewer contract

The runtime must expose a generic `ViewerLaneGroup` / `ViewerLaneTrack`
description alongside derived lanes. A group provides:

- stable group identifier;
- display label;
- ordered tracks;
- per-track relative height;
- optional value-display representation.

Each track provides its ordinary derived-lane payload. The viewer renders the
group from this metadata only; it never infers grouping from lane names.

For example, the UART-specific builder can declare a group named after its
decoder instance with a `bits` track and a `data` track. The viewer only sees
two generic tracks, their order, and their heights.

## Compatibility

Saved graphs may retain legacy sockets internally where needed, but migrations
must be expressed at node restore/load boundaries and reported as warnings.
They must not be hidden through generic viewer or compiler special cases.

## Consequence

The current temporary UART name-based grouping (`uart_data_lane_name` and
related `Bits`/`Data` matching) is explicitly non-compliant and must be
removed when the generic lane-group contract is implemented.
