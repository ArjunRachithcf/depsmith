---
status: proposed
---

# External adapters are subprocesses speaking a versioned JSON-lines protocol that mirrors the adapter seam

ADR 0001 deferred out-of-tree adapters but kept every seam type owned and serialisable for them; this adds the bridge: an external adapter is an executable that depsmith starts once per run and talks to over stdin and stdout, one JSON request (`{"id", "method", "params"}`) and one reply (`{"id", "result"}` or `{"id", "error": {"kind", "message"}}`) per line, with one method per `Adapter` method (`detects`, `detects_in`, `inventory`, `select`, `declarations`, `rewrite`, `availability`, `pin`, `prepare`) and the manager name, capabilities and tools carried by the `AdapterSpec` the `hello` handshake returns. A repository enables external adapters in `depsmith.toml`, by name (`depsmith-adapter-<name>` on `PATH`) or by path, under the trust model it already has for `[options.tools]`: depsmith does not defend against an untrusted repository, whose native tools can run its code anyway, and never downloads an adapter.

## Consequences

- The protocol version is one integer, exchanged in `hello` and raised only for incompatible changes; depsmith refuses a version it does not speak and names both. Within a version, fields may be added and unknown fields are ignored.
- Error `kind`s are depsmith's error categories (invalid, operation, stale, policy, I/O), given a serialised form for the protocol, so failures keep their exit statuses.
- External adapters share discovery, staging, suggestions, acceptance, scanning, init and doctor with in-tree ones; paths are relative to the stage or repository root depsmith passes, and edits come back through `rewrite`. An external manager name must not shadow an in-tree one.
- `depsmith adapter check <name>` runs `conformance::check` against the executable, from a fixture directory whose files are copied as the repository plus a small manifest naming the target and the expected declarations (the fields of `conformance::Fixture`).

## Considered options

- **Dynamic libraries**: rejected; Rust has no stable ABI, and an adapter's crash would take the engine with it.
- **WebAssembly components**: rejected for now; adapters run native tools in the stage, which a WASI sandbox would have to grant back piece by piece.
- **One process per call**: rejected; start-up cost would multiply by targets and methods, and adapters could not cache registry data within a run.
- **Discover every `depsmith-adapter-*` on `PATH`**: rejected; which managers own a repository's files should not depend on what happens to be installed.
- **Adapters only from `PATH`, never a repository path** (the first plan): rejected; stricter than `[options.tools]` without making untrusted repositories safe.
