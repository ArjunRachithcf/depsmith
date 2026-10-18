# Reviewing and applying

## What a proposal contains

Preparing a proposal resolves each selected target in its own **stage**; the
repository is never modified while preparing. A proposal records:

- **file changes**: the exact new content of each file, with a diff;
- **dependency changes**: every package added, removed or changed between the
  current resolution (the **baseline**) and the new one (the **candidate**),
  including transitive dependencies and every configured platform. A package
  the target does not declare names its **introducers**, the declared
  dependencies that pull it in, with the shortest dependency path from each
  (from the lock graph; Pixi, uv, Cargo and npm). The text report lists these
  as `pulled in by …`;
- **suggestions** about declared constraints, **unresolved references**,
  **validation** levels completed, **scan reports** when requested, and
  **failures** for targets that could not be prepared.

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
copied. Select the repository root, not a subdirectory. Nested repositories,
submodules, external object stores, sparse checkouts, external attributes,
configuration includes or filters, and worktree-specific configuration are
rejected with a configuration error. Large histories add copying and hashing
cost.

Repository symlinks and local dependencies outside the repository are
rejected.

## Backend processes

Package managers and scanners run without a shell, with a time limit
(`timeout_seconds`, default 300). A timeout or interrupt kills the backend
together with every process it started: on Unix through its process group, on
Windows with `taskkill /T`. Interrupting the CLI exits with 130. Failures
include the last 40 lines of the backend's stderr with credentials redacted
(URL passwords, token-like parameters, authorization headers, GitHub tokens and
values of secret-named environment variables); stdout is never echoed.
