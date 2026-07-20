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

Generic storage accepts explicit working, persistent-cache, and session-repository directories.
The native application platform owns the application namespace and operating-system directory
policy, then passes resolved paths through configuration. Generic crates do not inspect host
environment variables to choose an application location.

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

## Module layout

The workspace uses an owner-facade module layout.

### Source structure

- Module declarations occur only in `lib.rs`, `main.rs`, and `mod.rs` files.
- Test modules are the only exception. They may be declared in any Rust file, but every test
  module name contains `tests`.
- Modules are private by default. An owning `mod.rs` or crate `lib.rs` selectively re-exports the
  symbols that form its internal or external contract.
- A public module is an intentional API namespace, not a way to make its implementation easier
  to import. Public modules are limited to the allowlist below.
- Every public module is directory-backed and has a `mod.rs`; public file modules such as
  `pub mod capture;` backed by `capture.rs` are not permitted.
- A `mod.rs` contains module documentation, attributes on module declarations or re-exports,
  module declarations, and re-exports only. Structs, enums, traits, implementations, functions,
  constants, type aliases, executable macro bodies, and other implementation code live in leaf
  files.
- Target selection uses attributes on complete module declarations and re-exports in an allowed
  root file. It does not require inline implementation modules or executable selection macros in
  a `mod.rs`.

### Visibility through facades

Leaf symbols used only in their defining module are private. A symbol re-exported for another
module in the same crate is `pub(crate)` at its definition and at the owning facade. A supported
cross-crate or plugin contract is `pub` at its definition and is publicly re-exported from an
allowlisted public module or the crate root.

The layout does not use `pub(super)` or `pub(in ...)`. The facade path communicates the
owner and intended dependency direction, while `pub(crate)` provides the visibility required to
form an internal re-export. `pub` never means merely "used by another file"; it always denotes a
supported external contract.

Struct fields are private by default. Behavioral and invariant-owning structs expose methods.
Plain record types intended for construction or pattern matching may expose their data fields,
but those fields use one consistent visibility matching the record contract. A struct does not
mix private, `pub(crate)`, and `pub` data fields; read-only access uses methods instead.

### Public-module allowlist

All modules absent from this table are private and expose supported symbols through their
nearest owning facade. The allowlist names canonical public namespaces.

| Crate | Public modules | Rationale |
| --- | --- | --- |
| `signal_processing` | `capture`, `live_capture`, `live_capture_store`, `derived_word_store`; native-only `waveform_index` | These are substantial, independent generic capture and storage domains. Runtime plumbing such as ports, senders, receivers, scheduling, workers, errors, and pipeline implementation remains private behind root re-exports. |
| `logic_analyzer_processing` | `live_capture`, `nodes`, `nodes::decoders`, `nodes::logic`, `nodes::sinks`, `nodes::sources` | Acquisition and the four concrete node families are useful API namespaces. Individual decoder, logic-node, sink, device, source, transport, and format implementation modules remain private and are re-exported by their family facade. |
| `logic_analyzer_graph` | `nodes`, `nodes::decoders`, `nodes::logic`, `nodes::sinks`, `nodes::sources` | Concrete graph-node types use the same four family namespaces as their processing implementations. `compiler`, builder, definition, lowering, migration, registry, platform, and presentation modules remain private; supported compiler and plugin contracts are re-exported at the crate root. |
| `node_graph` | none | The reusable widget exposes one curated crate-root API; model, runtime, support, API, and widget implementation modules remain private. |
| `logic_analyzer_viewer` | none | The reusable viewer exposes one curated crate-root API; drawing, sampling, input, cursor, lane, worker, and indexing modules remain private. |
| `logic_analyzer_ui` | none | The application-composition crate exposes only its host-facing crate-root facade. |
| `input_bindings`, `panel_layout`, `trigger_editor`, `widget_support` | none | Each crate already represents one cohesive public component and does not need a second namespace level. |
| Native/web application crates and example plugins | none | Binary integration and plugin registration are crate-root entry points; implementation modules remain private. |

Changing this allowlist is an API-design decision. A new public module requires a documented
domain boundary, more than import convenience, and review of its native and wasm surface.

### Enforcement

The source-structure check in CI rejects module declarations outside the
allowed root files, non-test exceptions, test module names without `tests`, public file modules,
implementation items in `mod.rs`, public modules outside the allowlist, and occurrences of
`pub(super)` or `pub(in ...)`. The existing `-D unreachable-pub` check remains enabled.

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

`AppManager` is one such facade. Its public type and operations are identical on every target;
whole implementation files delegate to the threaded native manager or cooperative wasm manager.

## Enforcement

Architecture tests protect prohibited dependency and terminology directions. Workspace checks
run the compiler's `unreachable_pub` lint, and new warnings are treated as visibility defects.
Public API review includes re-exports, associated items, fields, variants, native and wasm
surfaces, and every type appearing in a public trait signature.
