---
status: proposed
---

# Staging keeps in-repository symlinks, leaves out what it cannot copy, and fails only the targets whose directory holds it

depsmith resolves each target in a stage: a copy of the repository's inputs in a temporary directory, so a native tool can never write to the real tree. Until now, one symlink, nested Git repository or submodule anywhere in the repository, or one tracked file under a directory name staging skips (such as `src/target/`), made listing the inputs fail. That failed `discover` and every target. In a corpus of 500 public repositories, about 30% contain a tracked symlink, usually `CLAUDE.md -> AGENTS.md`, so depsmith could not even list their targets. So staging now handles each of these cases, and anything it cannot copy fails only the targets whose directory holds it.

## Consequences

- **Symlinks** whose target stays inside the repository, both as written and as the file system resolves it through other links, are recreated in the stage as the same symlink. They are fingerprinted by their target text, so retargeting a link makes a proposal stale. Three kinds of symlink are left out of the stage: one that points outside the repository, one that uses an absolute target, and one that points into another left-out path. This applies to links anywhere: in ignored directories, in directories staging skips, and in the directories above a managed input. On platforms where depsmith cannot create symlinks, in-repository links are left out too.
- **Nested Git repositories and submodules** are left out of the stage, directory and all. A repository root that is itself inside another repository is still rejected, because its Git state belongs to the outer repository.
- **Tracked files** are always staged, whatever their directory is called. A file Git tracks is a source, even under a name staging otherwise skips, such as `target` or `node_modules`.
- **Discovery never fails** on any of these. It lists the manifests it finds outside the left-out paths.
- **Per target:** a left-out path fails every target whose directory holds it, naming every such path and its reason. A target elsewhere in the repository is prepared as usual. A root target holds every path, so a submodule or an outside symlink anywhere fails it. That is deliberate: depsmith cannot tell whether the native tool would have needed what the stage left out, and failing is safer than a proposal resolved against an incomplete copy.
- **Rejected alternatives:**
  - Dereferencing links would stage content the repository does not have, and would hide a retargeted link.
  - Failing only targets whose manifest or managed inputs lie under a left-out path would keep root targets working, but a build that quietly needs the left-out content could then produce a proposal with only a note as a warning.
