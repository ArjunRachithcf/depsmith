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

An adapter declares its package manager and capabilities, recognises its
targets, maps requested package names to declared direct dependencies, reads
inventories from existing locks, and prepares a candidate inside a stage. It
never writes outside the stage; all writes to the repository belong to the
engine. Unsupported requests fail instead of being ignored. See the
`depsmith_core::adapter` module docs.

## Modules of `depsmith-core`

| Module | Responsibility |
|---|---|
| `lib` | `Engine`, discovery, preparation, `doctor`, scanning existing locks |
| `adapter` | The adapter contract and capabilities |
| `pixi`, `actions` | The Pixi and GitHub Actions adapters |
| `working_tree` | Listing, fingerprinting and staging repository files; path safety |
| `scm` | Data-only Git staging for SCM-derived versions |
| `transaction` | Applying proposals: stale checks, locking, journal, recovery |
| `scan`, `inventory` | Inventories, identities, Grype scanning and policy |
| `process` | Running backends: timeouts, process-tree cancellation, redaction |
| `constraint`, `pep440`, `conda_version`, `cutoff`, `pypi` | Constraint acceptance, version ordering, release-age cutoffs and PyPI evidence |
| `config` | `depsmith.toml` |
| `model` | Shared data types and errors |
