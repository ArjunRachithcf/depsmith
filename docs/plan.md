# depsmith, an extensible dependency updater — First Release Plan

## 1. Goal and release boundaries

Build a Rust package updater with a consistent local and CI workflow, plus a typed Python API through PyO3. Distribute it as a standalone executable, a pip package, and a conda-forge package.

**The first release targets a representative Pixi project:** update `pixi.lock`, suggest dependency-health improvements in `pixi.toml` and `pyproject.toml`, update GitHub Actions references (including commit-pin conversion), and optionally compare vulnerabilities before and after updates. It also ships Cargo, conda (conda-lock) and uv adapters built on the extensible adapter seam (§8).

Release 0.1.0 is a public alpha on PyPI plus a draft GitHub release with standalone executables. The conda-forge recipe is prepared and validated in CI; feedstock submission follows the release and does not block it.

The broader roadmap includes npm, vcpkg, prek/pre-commit, and additional ecosystems. Mainframe operating systems, installed-machine upgrades, external plugins, and PR management are outside the initial scope.

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

Interactive users confirm targets and are asked whether to save the selection as `targets` in `depsmith.toml` (default no; formatting preserved). Prompting happens only when no selection is saved, so an existing selection is never replaced. JSON, noninteractive and Python use never save. CI and Python callers supply targets or use saved configuration; ambiguity is an error.

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

Stay within the current major release line by default. Offer major upgrades separately. Preserve existing tag or commit-pinning style; a suggestion to convert tags to full commit pins is deferred to 0.2. Unresolvable custom references remain unchanged with an explanation.

**Staging and application**

Generate candidate files in a disposable workspace with the required local project sources and path relationships. Preserve working-tree content, including existing user edits. Detect references outside the staged boundary and reject operations that cannot be reproduced safely.

Preview semantic dependency changes and exact file diffs. Apply the reviewed candidate files without rerunning resolution, after verifying relevant input fingerprints.

Use an application journal and backups for recoverable multi-file writes. Refuse stale proposals; never overwrite concurrent edits. Shared-file conflicts block affected targets. Independent successful targets may still be applied after explicit partial-success selection.

Default validation checks resolution and manifest/lock consistency. Optional validation installs into a disposable environment. Configured project checks after installation are deferred to 0.2. Report exactly which validation levels completed.

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

Each target downloads its own database snapshot; sharing one download across targets is a later optimization.

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

Prepare the conda-forge recipe and release automation. Feedstock acceptance and publishing credentials are external release prerequisites.

**Native tools.** Package managers and Grype are separate executables. `depsmith init` checks the ones a repository's targets use and, only with consent (an interactive prompt per tool, `init --fetch-tools`, or `fetch_tools=True` in Python, which never prompts), installs each missing one from its pinned tested release: HTTPS only, sha256 verified before unpacking, confined unpacking, atomic install into a per-user tool cache. Each install is recorded with its sha256 in the repository's self-ignoring `.depsmith/tools.json`; a cached tool runs only while it matches that record at the pinned version, and is otherwise reported and offered again by `init`. Updates and scans never block on a missing tool. cargo and conda-lock are not downloaded. `doctor` explains every tool's source and version.

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

Tests run in three layers, in priority order:

1. **Offline CLI end-to-end** (every CI run, all four hosts, required): the built executable against disposable fixtures with a cross-platform scripted backend stand-in — check/apply/recheck exit codes, JSON stdout, stale proposals, and cancellation killing a backend's descendants on Unix and Windows.
2. **Native integration** (pull requests to and pushes on `main`, nightly, and as a gate in the Release workflow; informational on pull requests): real Pixi 0.80.0, uv 0.12.15, cargo, conda-lock 4.0.2 with micromamba 2.9.0 and Grype 0.119.0, pinned, plus pypi.org, crates.io and the GitHub API — `[project]` suggestion acceptance, mixed conda/PyPI across multiple platforms, Actions tags/SHA pins/reusable workflows/major suggestions, and scanner comparisons (a native Git-pin preservation scenario is deferred; offline tests cover the policy). Failed native tests are retried once; nightly failures open or update a `nightly-integration` issue. Linux runs everything; Windows and macOS run the Pixi and uv tests. A weekly **Latest tools** run repeats Integration once per tool with only that tool at its newest stable release published at least 7 days ago; it opens a pull request bumping each tool whose release passes (tested version, pinned downloads and Integration pin together), and keeps one `latest-tools` issue per tool that the release breaks, whose pinned checksum drifted, or that cannot be bumped automatically.
3. **CLI/Python parity**: one fixture through both interfaces yields equal proposals.

The **conda** and **uv** adapters ship in this release (§8). Conda locking uses conda-lock's artifact-exact unified YAML format, following conda-lock's own lock path (`environment.conda-lock.yml` is recognised). Reproduce any reported conda environment-creation failure before adding an ordering workaround; preserve it as a regression test if recovered.

## 6. Path to 0.1.0

**A. Repository gates and merge.** `main` is protected by a ruleset (signed commits, linear history, pull requests, required checks, CodeQL). Codecov tracks coverage history per flag and component and gates patch coverage (80% of changed lines; its project status needs a paid plan); the in-repo `coverage` check fails below 75% combined Rust and Python line coverage, which also keeps a gate if Codecov is unavailable. GitHub's Code Quality coverage uploads are not available to this personal repository. Changes land through pull requests using a *local signed fast-forward*: rebase and sign locally, push the branch, wait for required checks, then fast-forward `main` from the verified branch (GitHub's server-side rebase-merge cannot sign commits). Repository administrators bypass the update restriction for this step only. Required checks cover every offline job (Rust ×4, MSRV, wheels ×4, sdist, conda, prek hooks, CLI end-to-end, coverage); native integration is not required on pull requests. Repository hooks run through prek locally and in CI.

**B. Release scope (done).** Saved interactive selection, offline CLI end-to-end tests including Windows process-tree termination, the native integration workflow and CLI/Python parity are merged; the uv adapter, `depsmith init` tool provisioning and the Latest tools workflow were added before the release.

**C. Release.** Tag `v0.1.0` on `main`, run the Release workflow without publishing, review the artifacts, then publish only with explicit approval. PyPI trusted publishing and the `pypi`/`release` environments are configured beforehand. Submit the conda-forge feedstock afterwards.

**Later (0.2 and beyond).** Configured post-install project checks, interactive per-suggestion acceptance prompts, shared scanner database downloads, and an out-of-tree adapter bridge.

## 7. Documentation automation

Documentation is kept consistent with the code by reviewed sources plus AI passes that can only edit documentation.

- **Vocabulary:** `CONTEXT.md` is the glossary every document and prompt follows.
- **API docs:** public Rust items and the public Python API are documented (`missing_docs`, Ruff `D`, `cargo doc` with warnings as errors); examples on the main entry points run as tests. On each same-repository pull request, the Docstrings workflow finds changed symbols with codebase-memory-mcp and lets Claude (Sonnet 5) update only their doc comments and docstrings. A docs-only guard, the doc build and Ruff must pass before a docs GitHub App commits the change to the PR branch (GitHub-signed, re-running CI); `docs-guard` re-checks every such commit.
- **Wiki:** `docs/wiki/` is reviewed in pull requests and mirrored to the GitHub wiki on every push to `main`. The CLI and Python API reference pages are generated from `--help` and docstrings; `docs-reference` fails when they are stale or a wiki link is broken.
- **README and wiki drift:** a nightly job has Claude (Opus 5.5) audit `README.md` and `docs/wiki/` against the code and opens or updates one pull request when something drifted; failures open a tracking issue.
- **Credentials:** a Claude subscription token and the docs GitHub App's key are repository secrets; fork pull requests never receive them.

## 8. Extensible adapters

Folded into 0.1.0 (ADR 0001). A new package manager is one adapter module plus one registration line, and must pass a shared conformance suite.

- **Seam:** an adapter is described by an owned, serde-serialisable `AdapterSpec` (manager, ecosystems, discovery patterns, managed files, skipped directories, native tools, capabilities). It reports `Declaration`s, applies `Edit`s with `rewrite`, names registries as data (`RegistryConfig`), may `pin` movable references, and prepares candidates in the stage. Discovery, staging, `doctor`, tool paths (`--tool NAME=PATH`), suggestions and `--accept` are generic.
- **Ecosystems:** conda, PyPI, Cargo and GitHub Actions each have one version scheme (ordering, caps, restyling, explicit-requirement checks, pins) and one scan identity rule, shared by every manager that resolves them.
- **Registries:** `pixi search`, PEP 691 indexes, the crates.io sparse index, conda sharded repodata (CEP 16, full repodata as fallback) and an inline fixture for tests, all feeding one evidence format.
- **Adapters:** Pixi and GitHub Actions were ported without behaviour changes; Actions gained commit-pin suggestions and `--accept` to `@<sha> # vX.Y.Z`; Cargo targets each `Cargo.lock` owner with MSRV-aware resolution; conda targets `environment.yml` locked with conda-lock; uv targets each `uv.lock` owner (workspace roots), reading `[project]`, `[dependency-groups]` and `tool.uv.dev-dependencies` through a shared `pyproject` module, keeping Git pins and following uv's index priority.
- **Conformance:** `depsmith_core::conformance::check` runs the contract offline (spec round-trip, discovery, managed files, selection, capability enforcement, declarations round-tripping through `rewrite`). Each adapter also has a native live round trip in the Integration workflow.
- **Protocol-ready:** every seam type holds owned data only, so an out-of-tree subprocess bridge (`{method, params}` JSON lines) can be added later without redesign.
