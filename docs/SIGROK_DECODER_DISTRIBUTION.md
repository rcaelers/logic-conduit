# Sigrok Decoder Distribution

## Native application policy

LogicConduit hosts API-version-3 Sigrok Python decoder packages supplied through explicit decoder
search paths. Decoder packages are not embedded in the native application bundle. The catalog
shows each package's declared license beside its decoder name and preserves discovery failures as
diagnostics rather than silently omitting them.

Decoder scripts are trusted executable code. Selecting a search path authorizes Python packages
under that directory to run with the native application's permissions when they are discovered or
used. Search-path order is significant: the first successfully discovered package for a decoder
ID wins, and later duplicates are reported.

## Packaging boundary

The native package contains the PyO3 host and CPython integration, but no `libsigrokdecode` C
library and no third-party decoder collection. Packaging therefore does not combine LogicConduit's
MIT-licensed Rust code with decoder-package licenses. A future bundled collection requires an
inventory of every included package's declared license, Python dependencies, data files, native
extensions, subprocess use, and corresponding notices before the bundle is released.

The wasm application does not register the native Sigrok decoder node or CPython host. A future web
backend is a separate complete implementation boundary.

## Validation

Catalog tests cover ordered paths, duplicate IDs, missing directories, decoder metadata including
licenses, cache reuse, and explicit refresh. The ignored complete-tree performance test scans a
developer-supplied standard decoder tree and requires SPI discovery within thirty seconds. Generic
UI, compiler, viewer, and node-graph architecture tests reject Sigrok-specific host cases.
