# depsmith

depsmith prepares, reviews and applies dependency updates for repositories, through one Rust engine shared by a CLI and a Python API. This glossary fixes the words used for its concepts in code documentation, the README and the wiki.

## Projects and managers

**Package manager**:
A native tool that owns a kind of dependency declaration and resolves it, such as Pixi or GitHub Actions workflow references.
_Avoid_: Backend (except for the running process), tool

**Ecosystem**:
The namespace a package identity belongs to, such as conda, PyPI or GitHub Actions; one package manager can resolve several ecosystems.
_Avoid_: Manager, registry

**Target**:
One manifest or workflow file that depsmith updates, identified as `manager:path`, together with the files its package manager owns.
_Avoid_: Project, manifest (when the whole unit is meant)

**Adapter**:
depsmith's integration for one package manager, deciding which files form targets and how they are updated.
_Avoid_: Plugin, driver

**Capability**:
A behaviour an adapter declares as supported, unsupported or not applicable, such as package selection or install validation.
_Avoid_: Feature flag

**Direct dependency**:
A package named in a target's own declarations, as opposed to one resolved only because another package needs it.
_Avoid_: Top-level package

## Preparing an update

**Update**:
Re-resolving a target within its existing declared constraints.
_Avoid_: Refresh, bump

**Upgrade**:
Re-resolving selected direct dependencies while allowing their declared constraints to change.
_Avoid_: Major update

**Stage**:
A disposable copy of the repository in which package managers resolve, so the original checkout is never modified while preparing.
_Avoid_: Sandbox, temp dir, workspace (Pixi's word for a project)

**Candidate**:
The proposed state of a target after resolution in the stage: its new files and resolved packages.
_Avoid_: Result, output

**Proposal**:
The reviewable outcome of preparing one or more targets: exact file changes, dependency changes, suggestions, validation notes, scans and failures.
_Avoid_: Plan, patch, preview (which is how a proposal is shown)

**Dependency change**:
A package that is added, removed or moved to a different version or artifact between the current and candidate resolutions.
_Avoid_: Diff (reserved for file text)

**Validation**:
A check that a candidate is consistent and usable, recorded by the level actually completed, such as lock consistency or host installation.

**Failure**:
A target whose candidate could not be prepared, with the exit status category that explains why.
_Avoid_: Error (for per-target outcomes)

**Partial success**:
A proposal in which some targets succeeded and others failed; applying it requires explicit opt-in.

**Unresolved reference**:
A workflow reference left unchanged because no release can be matched to it, reported with the reason.
_Avoid_: Unknown action

## Constraints and suggestions

**Constraint**:
A declared version requirement for a direct dependency, such as a pin or an upper bound.
_Avoid_: Spec, range

**Suggestion**:
An evidence-backed report that a constraint excludes a newer release or is otherwise worth revisiting, naming the affected declaration.
_Avoid_: Recommendation, warning

**Evidence**:
The verifiable source behind a suggestion or identity mapping, such as an artifact URL with its SHA-256 or a release page.
_Avoid_: Proof, justification

**Acceptance**:
Rewriting a suggested constraint to admit the evidenced newer release, in the same style or as an explicit replacement, before resolving again.
_Avoid_: Apply (reserved for writing a proposal)

**Cooldown**:
A minimum age a release must reach before it may be selected; Pixi expresses its own form as a release-age cutoff.
_Avoid_: Quarantine, delay

**Git pin**:
A dependency resolved from a specific Git commit, preserved unless a Git refresh is explicitly requested.

## Applying

**Apply**:
Writing exactly the files a reviewed proposal contains, without resolving again, after confirming the inputs are unchanged.
_Avoid_: Commit, merge

**Stale proposal**:
A proposal whose recorded inputs no longer match the repository, so it must be prepared again rather than applied.
_Avoid_: Conflict, outdated

**Journal**:
The durable record of an in-progress apply that lets an interrupted apply be recovered.
_Avoid_: Log, backup

**Recover**:
Restoring the files recorded in a journal after an interrupted apply.
_Avoid_: Rollback, undo

## Scanning

**Inventory**:
The resolved packages of a target, with ecosystem, version, platform and artifact, as input for scanning.
_Avoid_: SBOM (the exported form), package list

**Identity mapping**:
A reviewed statement, with evidence, that a package in one ecosystem is the same software as an upstream identity used by vulnerability databases.
_Avoid_: Alias, name match

**Baseline**:
The current resolution of a target, scanned alongside the candidate with the same vulnerability database snapshot.
_Avoid_: Before scan, reference

**Scan report**:
The vulnerability findings for a target, classified against the baseline as introduced, resolved or remaining, with coverage of what could not be assessed.
_Avoid_: Audit

**Finding**:
One advisory matched to one package in an inventory, with its applicability.
_Avoid_: Vulnerability (the advisory itself), alert

**Unassessed**:
A package the scanner could not evaluate, reported as unknown and never as clean.
_Avoid_: Clean, skipped

**Suppression**:
A documented, scoped and optionally expiring decision to exclude a finding from policy gates while keeping it in the report.
_Avoid_: Ignore, allowlist entry

**Policy**:
A configured rule that rejects a proposal because of its findings, such as a severity threshold.
_Avoid_: Gate (for the configured rule)
