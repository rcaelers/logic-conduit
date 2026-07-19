# Responsibility and Visibility Design

## Design

Every module and crate exposes only capabilities that belong to its stated responsibility.
Visibility is an architectural contract: an item is public only when a consumer at that
visibility boundary is expected to depend on it.

Crate APIs use responsibility-oriented names and errors. Generic crates do not retain aliases,
error variants, helpers, or dependencies for a concrete capture format, device, protocol, node,
or presentation. Compatibility for a concrete feature is owned by that feature's crate and is
made explicit at its load or migration boundary.

## Ownership

The crate boundaries in `AGENTS.md` are enforced at both dependency and symbol level:

- `signal_processing` owns generic runtime, capture, storage, indexing, and derived-data
  contracts. Its public capture vocabulary is `Capture*`; it does not expose DSL, Sigrok, USB,
  decoder, graph-node, or UI terminology.
- `logic_analyzer_processing` owns concrete capture formats, devices, protocol decoders,
  processing nodes, and sinks. Format parsing and device-transport errors originate here and are
  mapped to generic runtime errors only where a generic trait requires it.
- `logic_analyzer_graph` owns concrete node definitions, lowering, builders, and registration.
  Definition defaults and lowering helpers remain crate-private unless plugin authors or another
  crate implement against a documented contract.
- Generic graph, viewer, compiler, runtime, and widget crates consume explicit metadata and
  capability contracts. They do not infer concrete behavior from names.
- Presentation helpers shared by widgets live in a neutral widget module or crate. Input-binding
  crates expose input and shortcut behavior, not unrelated menu layout policy.

Concrete aliases are declared beside their concrete implementation. A common abstraction module
does not import one implementation merely to publish a convenience alias.

## Visibility rules

Use the narrowest visibility that contains every intended consumer:

- private for implementation details used in one module;
- `pub(super)` for collaboration with the direct parent or sibling modules through that parent;
- `pub(crate)` for an internal crate contract;
- `pub` only for a supported cross-crate or plugin contract.

A `pub` item hidden below a private module is still an invalid declaration unless its wider
visibility is required by a public signature. Public re-exports are deliberate API decisions,
not a convenience for internal imports.

Public traits expose a complete implementable contract. Every type in their required method
signatures is publicly nameable from a stable path. Conversely, implementation seams that are
not supported extension points remain private, including their generic parameters and errors.

## Error boundaries

Generic errors describe failures at the abstraction boundary, such as I/O, invalid generic
indices, or malformed generic storage. Concrete parsers and transports own their detailed error
types and dependencies. When they implement a generic source or runtime trait, they translate a
concrete error into a generic boundary error without moving the concrete dependency into the
generic crate.

## Platform surfaces

Native and wasm public surfaces share the platform-neutral data model. Native-only filesystem,
USB, mmap, worker, export, and host-integration capabilities are selected as complete modules or
registry entries. A platform facade exposes a complete contract; consumers do not depend on an
unnameable backend type or a target-dependent collection of incidental helpers.

## Enforcement

Architecture tests protect prohibited dependency and terminology directions. Workspace checks
run the compiler's `unreachable_pub` lint, and new warnings are treated as visibility defects.
Public API review includes re-exports, associated items, fields, variants, native and wasm
surfaces, and every type appearing in a public trait signature.
