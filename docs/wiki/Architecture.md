# Architecture

## Crates

| Crate | Role |
|---|---|
| `depsmith-core` | The engine: discovery, adapters, staging, proposals, scanning and applying. |
| `depsmith-cli` | The `depsmith` executable: arguments, prompts, reports and exit statuses. |
| `depsmith-python` | PyO3 bindings (`depsmith._native`) behind the typed Python package in `python/depsmith`. |

The CLI and Python API are thin layers; all behaviour lives in the core.

## Flow

1. **Discover:** walk the repository (respecting ignore rules and skipping
   environment and cache directories); each **adapter** claims the files that
   are its **targets**.
2. **Validate before work:** options, adapter **capabilities**, and
   `--package`/`--accept` names against declared direct dependencies are
   checked before any package manager runs.
3. **Stage:** copy the repository's inputs (and Git data, for SCM versions)
   into a disposable **stage** and fingerprint them.
4. **Prepare:** the adapter resolves the target in the stage with its native
   package manager and returns a **candidate**: produced files, baseline and
   candidate inventories, suggestions, unresolved references and validation
   notes.
5. **Scan (optional):** build CycloneDX inventories with verified identities
   and scan baseline and candidate with one Grype database snapshot.
6. **Propose:** the engine diffs the candidate files against the repository into
   a **proposal**, with dependency changes and per-target failures.
7. **Apply:** take the repository lock, check the fingerprints are unchanged,
   journal the writes, and write exactly the proposed files; **recover**
   restores the original files of an interrupted apply.

## Adapter contract

An adapter describes its package manager as data and resolves inside a stage;
the engine does everything generic. It never writes outside the stage; all
writes to the repository belong to the engine. Unsupported requests fail
instead of being ignored. See the `depsmith_core::adapter` module docs.

- `spec()`: an `AdapterSpec` with the manager name, the **ecosystems** it
  resolves, discovery patterns, managed files (locks and native configuration
  staged even when ignored), directories never walked, native tools
  (`ToolSpec`) and capabilities. Discovery, staging, `doctor` and tool paths
  come from specs.
- `detects` / `detects_in`: whether a file is a target; `detects_in` may look
  at other files, as Cargo does to tell a workspace member from a lock owner.
- `select`: requested `--package` names to declared spellings.
- `declarations`: where each direct dependency and its requirement are
  written (`Declaration`: ecosystem, package, requirement, file, location).
- `rewrite`: apply `Edit`s to those declarations in the stage, keeping
  comments and layout.
- `availability`: the registry of each ecosystem and any release-age policy,
  as data (`RegistryConfig`).
- `pin`: an immutable replacement for a movable reference, such as the commit
  of an action's release tag.
- `inventory` and `prepare`: read a lock, and resolve a candidate.

Suggestions and `--accept` are generic: the engine reads declarations, asks
the ecosystem's **version scheme** whether a requirement can exclude newer
releases and how to restyle it, looks releases up in the registry, applies
accepted edits through `rewrite` before the adapter resolves, and suggests
from the resolved stage. Ecosystem rules (version schemes and scan
identities) live in `ecosystem` and are shared by every manager that resolves
that ecosystem.

## Adding a package manager

1. Write one module implementing `Adapter`, starting from `spec()`; reuse an
   existing ecosystem when the manager resolves one (conda, PyPI, Cargo), or
   add its version scheme and identity to `ecosystem`.
2. Implement `declarations` and `rewrite` to get suggestions and `--accept`
   for free; describe registries in `availability`. A `Fixture` registry
   keeps tests offline.
3. Register the adapter in `Engine::default`. If its native tool ships
   standalone release binaries, pin them (URL and sha256 per host) in
   `provision.rs` so `depsmith init` can install it. For CI, pin the tested
   version in `.github/tool-versions.json`, install it in the Integration
   workflow, and describe the tool in `TOOLS` in `scripts/tool-drift.py` so
   the weekly Latest tools run tracks its releases.
4. Test it with `depsmith_core::conformance::check`, which runs the contract
   offline: spec round-trip and shape, discovery, managed files, selection,
   capability enforcement, and declarations round-tripping through `rewrite`.
   Add a native live test (ignored by default) for a preview → accept → apply
   → recheck round trip.

Every seam type is owned and serde-serialisable, so a later out-of-tree
adapter can speak the same contract over a subprocess bridge of JSON lines
(`{"method": ..., "params": ...}`) without redesign.

## Modules of `depsmith-core`

| Module | Responsibility |
|---|---|
| `lib` | `Engine`, discovery, preparation, `doctor`, scanning existing locks |
| `adapter` | The adapter contract, specs and capabilities |
| `conformance` | Contract checks every adapter's tests run |
| `constraints`, `ecosystem` | Declarations, registries, generic suggestions and acceptance; version schemes and scan identities |
| `pixi`, `actions`, `cargo`, `conda`, `uv` | The Pixi, GitHub Actions, Cargo, conda and uv adapters |
| `pyproject` | PEP 508 requirement lists of `pyproject.toml`, shared by the Python-aware adapters |
| `provision` | Pinned tool downloads, the tool cache and per-repository tool records (`init`) |
| `http` | The shared HTTP client |
| `working_tree` | Listing, fingerprinting and staging repository files; path safety |
| `scm` | Data-only Git staging for SCM-derived versions |
| `transaction` | Applying proposals: stale checks, locking, journal, recovery |
| `scan`, `inventory` | Inventories, identities, Grype scanning and policy |
| `process` | Running backends: timeouts, process-tree cancellation, redaction |
| `constraint`, `pep440`, `conda_version`, `cutoff`, `pypi` | Constraint acceptance, version ordering, release-age cutoffs and PyPI evidence |
| `config` | `depsmith.toml` |
| `model` | Shared data types and errors |
