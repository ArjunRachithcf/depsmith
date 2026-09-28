# depsmith, an extensible dependency updater — First Release Plan

## 1. Goal and release boundaries

Build a Rust package updater with a consistent local and CI workflow, plus a typed Python API through PyO3. Distribute it as a standalone executable, a pip package, and a conda-forge package.

**The first release targets a representative Pixi project:** update `pixi.lock`, suggest dependency-health improvements in `pixi.toml` and `pyproject.toml`, update GitHub Actions references, and optionally compare vulnerabilities before and after updates.

Conda-family and uv adapters follow this first release. The broader roadmap includes npm, vcpkg, prek/pre-commit, and additional ecosystems. Mainframe operating systems, installed-machine upgrades, external plugins, and PR management are outside the initial scope.

## 2. Architecture and public interfaces

Use a Cargo workspace with separate components for the Rust core, built-in adapters, CLI, and PyO3 bindings. Keep update logic in the core and adapters; CLI and Python interfaces share the same policies and results.

**Manager adapters**

Each adapter provides discovery, capability reporting, prerequisite checks, dependency inventory, candidate generation, and candidate validation. Register adapters centrally without adding manager-specific branches to orchestration.

Capabilities explicitly describe support for targeted updates, constraint changes, cooldowns, Git references, platforms, and lockfile generation. Unsupported requested behavior produces an actionable error rather than silently relaxing policy.

Use installed native tools for resolution and lockfile generation. Share subprocess handling, cancellation, timeouts, diagnostics, credential redaction, and version checks.

**Separate extension interfaces**

- Dependency-health rules produce findings, explanations, evidence, and optional proposed edits.
- Inventory exporters describe resolved packages, provenance, platforms, and dependency relationships.
- Vulnerability scanners consume inventories and return findings plus coverage information.

**CLI and Python**

Use `depsmith` and `depsmith` as working executable/module names, subject to registry availability.

Provide CLI operations for `discover`, `check`, `update`, `scan`, and `doctor`. `update` previews changes interactively; noninteractive application requires explicit flags and unambiguous targets.

Expose synchronous, typed Python operations for discovery, proposal preparation, scanning, and application. Return structured objects and typed exceptions; never prompt or terminate the Python process. Release the GIL during blocking work.

Core public models include `Target`, `UpdateOptions`, `Proposal`, `DependencyChange`, `Suggestion`, `ScanReport`, and `ApplyResult`. Reports include schema versions and per-target outcomes.

Store optional repository configuration in `depsmith.toml`. Explicit command/API options override repository configuration; native manager configuration remains authoritative for ecosystem behavior unless explicitly overridden.

## 3. Update and review behavior

**Discovery and selection**

Discover supported projects beneath the selected root, respecting ignored directories and avoiding environment/cache directories. Show nested projects, overlapping ownership, and untracked candidate manifests explicitly.

Interactive users confirm targets and may save their selection. CI and Python callers supply targets or use saved configuration; ambiguity is an error.

Support whole-project updates and selected direct dependencies. Show all resulting transitive changes.

**Pixi and manifest suggestions**

- Preserve existing constraints, platform targets, feature/environment definitions, channels, local paths, and intentional pins by default.
- Update all configured target platforms; a failure on one blocks that project’s proposal.
- Preserve Git resolutions unless explicitly selected for refresh.
- Offer constraint-changing upgrades separately. Preserve constraint style where the transformation is unambiguous; require an explicit replacement for complex requirements.
- Limit dependency-health suggestions to blocked upgrades and verifiable dependency inconsistencies. Every suggestion names its evidence and affected declaration.
- Accepting a suggestion regenerates the candidate resolution, enabled scans, and final preview.
- Offer an optional common cooldown setting. Reject an explicit cooldown request when the adapter cannot enforce it; otherwise preserve native release policies.

**GitHub Actions**

Initially update remote `uses:` references in workflow files, including remote reusable workflows. Leave local actions, Docker references, script contents, runner labels, and action inputs unchanged.

Stay within the current major release line by default. Offer major upgrades separately. Preserve existing tag or commit-pinning style; conversion to full commit pins is an explicit suggestion. Unresolvable custom references remain unchanged with an explanation.

**Staging and application**

Generate candidate files in a disposable workspace with the required local project sources and path relationships. Preserve working-tree content, including existing user edits. Detect references outside the staged boundary and reject operations that cannot be reproduced safely.

Preview semantic dependency changes and exact file diffs. Apply the reviewed candidate files without rerunning resolution, after verifying relevant input fingerprints.

Use an application journal and backups for recoverable multi-file writes. Refuse stale proposals; never overwrite concurrent edits. Shared-file conflicts block affected targets. Independent successful targets may still be applied after explicit partial-success selection.

Default validation checks resolution and manifest/lock consistency. Optional validation installs into a disposable environment and runs configured project checks. Report exactly which validation levels completed.

## 4. Vulnerability scanning, CI, and distribution

**Vulnerability scanning**

Scanning is opt-in and required as a supported feature for the first release. Once requested, scanner execution failure blocks the affected proposal.

Use Grype as the initial integration candidate, with an explicit identity-enrichment layer and an acceptance gate before adoption:

- Build inventories from resolved lock data, including transitive dependencies and target platforms.
- Establish upstream identities from verifiable public metadata, supplemented by a small reviewed override set with provenance.
- Do not infer identity solely from matching package names.
- Preserve conda channel, version, build, platform, and artifact identity alongside upstream mappings.
- Distinguish advisory matches, uncertain build/backport applicability, and unsupported or unmapped packages.
- Compare baseline and candidate with the same vulnerability database snapshot and matching configuration.
- If no baseline lock exists, report a candidate-only scan; do not invent a resolved baseline.

Validate the integration against known vulnerable, fixed, misleading-name, and unmapped fixtures. Unsupported coverage remains unknown, never “clean.” If the candidate fails these requirements, scanner integration blocks release pending a revised technical design.

Reports distinguish introduced, resolved, and remaining findings. Default behavior reports findings; optional policies block by severity across all candidate findings or newly introduced findings. Allow scoped, documented suppressions while retaining suppressed findings in reports.

**CI contract**

Provide prompt-free check and apply modes, JSON output, Markdown summaries, and diagnostics on stderr. Keep stdout valid JSON when requested.

Define exit statuses:

| Code | Meaning |
|---|---|
| 0 | Operation succeeded; check found no pending changes |
| 1 | Check found available changes |
| 2 | Invalid configuration, arguments, or ambiguous targets |
| 3 | Tool, resolution, validation, or scanner failure |
| 4 | Configured policy rejected the proposal |
| 5 | Partial success |

Noninteractive partial application requires explicit opt-in. Resolve, scan, and apply within one job; portable executable proposals are deferred. CI examples cover GitHub Actions and a provider-neutral shell job. Existing pipeline steps own commits and PRs.

**Packaging**

Use Maturin for PyO3 wheels and source distributions, with Python type stubs. Default to CPython 3.10+ and conventional GIL-enabled builds initially.

Build and test standalone binaries and Python packages on Linux, Windows, and macOS. Initially target Linux/Windows x86-64 and macOS Intel/Apple Silicon.

Prepare the conda-forge recipe and release automation. Feedstock acceptance and publishing credentials are external release prerequisites. Native backend tools remain separately installed; `doctor` explains missing or incompatible prerequisites.

## 5. Implementation sequence and acceptance tests

1. **Core and adapter contract:** implement discovery, configuration, structured results, tool execution, and a fake adapter that exercises the complete lifecycle.
2. **Pixi workflow:** implement staged lockfile updates, targeted selections, manifest suggestions, exact application, and recovery.
3. **GitHub Actions adapter:** demonstrate that a different ecosystem can reuse orchestration, reporting, and application without special cases in the core.
4. **Security integration:** implement inventory identities, evidence-backed mappings, before/after scans, coverage reporting, and policy gates.
5. **Python, CI, and release packaging:** expose the shared core, verify interface parity, build artifacts, and document complete local/CI examples.

Release tests must cover:

- Mixed conda/PyPI projects, editable local builds, Git pins, multiple platforms, and intentional version constraints.
- Regeneration after accepting suggestions; transitive changes and solver conflicts.
- Dirty working trees, stale proposals, shared files, cancellation, interrupted writes, and recovery.
- Action tags, commit pins, reusable workflows, missing releases, and major-version separation.
- Scanner failures, missing baselines, identity ambiguity, vulnerability changes, and policy rejection.
- Equivalent CLI/Python outcomes, noninteractive behavior, exit statuses, and installation from built wheels/conda packages.
- Representative project fixtures and an integration run in a disposable copy. Production execution must not depend on a local checkout or private credentials.

After this release, add **conda-family**, then **uv** adapters. Conda locking uses an established artifact-exact YAML format, preferring `environment.conda-lock.yml` when required for native compatibility. Reproduce any reported conda environment-creation failure before adding an ordering workaround; preserve it as a regression test if recovered.
