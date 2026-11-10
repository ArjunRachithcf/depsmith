# Reviewing and applying

## What a proposal contains

Preparing a proposal resolves each selected target in its own **stage**; the
repository is never modified while preparing. A proposal records:

- **file changes**: the exact new content of each file, with a diff;
- **dependency changes**: every package added, removed or changed between the
  current resolution (the **baseline**) and the new one (the **candidate**),
  including transitive dependencies and every configured platform. Pixi
  environments that now share one version of a package each list a change to
  it, not a removal. A package the target does not declare names its
  **introducers**, the declared dependencies that pull it in, with the
  shortest dependency path from each (from the lock graph: Pixi, uv, Cargo,
  conda and npm). The text report lists these as `pulled in by …`;
- **suggestions** about declared constraints, **unresolved references**,
  **validation** levels completed, **scan reports** when requested, and
  **failures** for targets that could not be prepared.

A target whose resolution would move a package to an older version is **held
back**: it proposes no change, each downgrade is listed as an unresolved
reference (`cpp_demangle 0.5.1 -> 0.4.5`: no upgrade fix for now), and a
validation note says so. That includes a Pixi environment moving to an older
version another environment already locks. `--allow-downgrades` proposes such a resolution
anyway and names each downgrade in the validation notes.

## Applying exactly what was reviewed

Applying writes the reviewed files without resolving again, and only while the
repository still matches the inputs the proposal was prepared from. If any
input changed, including Git metadata such as tags or the index, the proposal
is **stale**: nothing is written and it must be prepared again. Uncommitted
edits are never overwritten.

Targets that write the same file conflict and block preparation. When some
targets failed, applying requires explicit opt-in (`--allow-partial`, or
`apply(allow_partial=True)` in Python) and then writes only the successful
targets (exit 5, partial success).

## Interruptions and recovery

Apply and recover hold an operating-system lock on `.depsmith/lock`, so
concurrent operations on one repository fail fast; the lock is released if a
process dies. Multi-file writes are recorded in a **journal**, published
atomically (written, synced, then hard-linked into place), so a crash never
leaves a partial journal; this needs a filesystem with hard links.

After an interrupted apply, `depsmith recover --root PATH` restores the files,
but only if each still holds either its old or its proposed content;
concurrent edits are never overwritten.

## Staging Git repositories

For Git repository roots and ordinary linked worktrees, the stage keeps Git
history, tags, the index and dirty source files, so SCM-derived package
versions are calculated normally. Hooks, remotes and credentials are not
copied. Select the repository root, not a subdirectory. External object
stores, sparse checkouts, external attributes, configuration includes or
filters, and worktree-specific configuration are rejected with a
configuration error. Large histories add copying and hashing cost.

Symlinks that stay inside the repository are recreated in the stage as the
same links. A symlink that points outside the repository (or uses an absolute
target, or points into another left-out path), a nested repository and a
submodule are **left out** of the stage. Discovery still lists the other
targets, but a left-out path fails every target whose directory holds it,
naming each such path and why; a target at the repository root holds them
all. Local dependencies outside the repository are rejected.

## Backend processes

Package managers and scanners run without a shell, with a time limit
(`timeout_seconds`, default 300). A timeout or interrupt kills the backend
together with every process it started: on Unix through its process group, on
Windows with `taskkill /T`. Interrupting the CLI exits with 130. Failures
include the last 40 lines of the backend's stderr, under its error line when
those hold none, with credentials redacted
(URL passwords, token-like parameters, authorization headers, GitHub tokens and
values of secret-named environment variables); stdout is never echoed. A
backend that writes more than 32 MiB to stdout fails rather than having its
output cut.
