---
status: proposed
---

# Updates are explained and steered by each target's lock graph, and package families move together within their constraints

A proposal lists what changed but not why: a transitive package that moved, a direct dependency that stayed behind, or CUDA packages that must move together all leave the reviewer guessing. So every adapter exposes its target's **lock graph**, read from the lock it already writes, and the engine uses that graph in three ways:
- it names each changed package's **introducers**;
- it reports the **blocker** that keeps a package below a newer release;
- it updates a **package family** as one selection.

Constraints still change only through acceptance. A family update is an ordinary update of more packages; it is never a workaround that edits declarations or overrides the platform, such as setting `CONDA_OVERRIDE_CUDA`.

## Consequences

- **The lock graph is read from each lock's own records**, per platform. ADR 0004's dependency paths use the same graph.
  - pixi.lock: conda `depends` and PyPI `requires_dist`, with requirements.
  - package-lock.json: per-package `dependencies`, with requirements.
  - conda-lock: per-package `dependencies`, with requirements.
  - Cargo.lock and uv.lock: dependency names only, with no requirements.
  - A native tree command (`cargo tree`, `uv tree`, `npm ls`, `pixi tree`) is used only where a lock records no edges.
- **Introducers.** Each dependency change of a package the target does not declare names the direct dependencies whose dependency paths reach it. The text report shows those names; JSON adds the full paths.
- **Blockers.**
  - A blocker is reported for each declared dependency, and for each package family member, held below the newest available release. Transitive packages are reported only with `--explain`, because availability has to be looked up for each package.
  - A blocker names:
    - a declaration, citing its suggestion and `--accept`;
    - another package's requirement, quoting it;
    - a platform requirement, such as a declared CUDA version or minimum Rust.
  - Where the lock records no requirements (Cargo.lock, uv.lock), they come from the registry's metadata for the locked parent release. Where neither has them, the blocker is reported as unknown.
- **Package families** are declared in `depsmith.toml` as `[[families]]`, each with an anchor and optional extra members.
  - Members are the anchor plus every package whose recorded requirements constrain it, plus the extras.
  - Built-in families can be overridden. The first is `cuda`, anchored on `cuda-version`. `pycuda` is an extra member, because it pins CUDA through its build rather than through `cuda-version`.
- **`--family NAME`** updates every member. `--package` on a member expands to its whole family, and the proposal says so. Both use each adapter's precise selection (`pixi update`, `uv lock --upgrade-package`, `cargo update -p`, `npm update`).
  - An adapter must show, in its live tests, that its tool moves a transitive member selected this way; until then, its family updates are reported as blocked.
  - A family stays within its constraints. Moving past them, such as from CUDA 12 to 13, is a suggestion naming the blocker, accepted with `--accept`.
- **Tests use mocked lock graphs**: a pixi.lock with a CUDA family, and a uv.lock and a package-lock.json with transitive moves. They need no GPU or network. The live tests cover only the native selection of transitive members.
- **Implementation order:**
  1. the lock graph and introducers;
  2. blockers and `--explain`;
  3. package families, `--family` and `--package` expansion.

## Considered options

- **Faking the platform to resolve CUDA packages** (`CONDA_OVERRIDE_CUDA` from the declared version): rejected. The solver already resolves against the declared CUDA version, and overriding it would hide a real platform blocker instead of reporting it.
- **Native tree commands as the main source**: rejected. They need the resolved environment or network access, they differ per tool, and the lock already records the same edges for the state being reviewed.
- **Calling a package family a "group"**: rejected. It clashes with dependency groups in pyproject.toml (PEP 735) and with uv's `--group`.
