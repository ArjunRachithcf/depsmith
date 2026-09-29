# depsmith

Prepare, review, and apply repository dependency updates with a Rust CLI and a
PyO3 Python API. The current alpha supports **Pixi lockfiles and GitHub Actions**.
Conda-family, uv, npm, vcpkg, and prek/pre-commit adapters remain on the roadmap.

## Build and install

From a checkout, with Rust 1.89+ and Python 3.10+:

```sh
python -m pip install .
depsmith --version
```

For a standalone executable, use `cargo build --release --locked -p depsmith-cli`.
For a wheel, use `maturin build --release --locked --out dist` after installing
Maturin. Install Pixi separately; install Grype separately if using `--scan`.
`depsmith doctor` (or `--json`) reports each executable's version as
`tested`, `untested` or `unavailable` — only Pixi 0.80.0 and Grype 0.119.0 have
been exercised; other versions are untested, not assumed incompatible — and lists
each adapter's capabilities as `supported`, `unsupported` (with a hint) or
`not-applicable`. Requesting an unsupported option fails with exit 2 before any
backend runs; an inapplicable option (e.g. `--install` for a workflow target) is
recorded in the proposal's validation list, except `--cooldown-days`, which fails
because ignoring it would relax policy.
Registry publication has not been performed; the package name is provisional.

## Preview and apply

```sh
depsmith discover --root /path/to/project --json
depsmith check --root /path/to/project --target pixi:pixi.toml --json
depsmith update --root /path/to/project --target pixi:pixi.toml --apply --yes --non-interactive
```

Discovery returns target IDs. A Pixi project in `pyproject.toml` uses
`pixi:pyproject.toml`; a workflow uses, for example,
`github-actions:.github/workflows/ci.yml`. Pass multiple `--target` flags or
`--all` explicitly. An ordinary Python project without `[tool.pixi]` is not yet
an update target.

`check` prepares a disposable candidate and does not write project files.
`update` can show an interactive preview; CI must use `--apply --yes
--non-interactive` to apply. Updates preserve manifest constraints by default.
To request a constraint-changing Pixi upgrade, add `--upgrade --package NAME`.
`--package` (repeatable) accepts only direct dependencies declared by a selected
target: Pixi dependency tables, `[project]` dependencies for `pyproject.toml`, or
remote repositories used by a workflow. Unknown names fail with exit 2 before any
backend runs; targets that declare none of the names are skipped. PyPI names match
after PEP 503 normalization; conda names match case-insensitively only.
Git resolution changes require `--refresh-git`. Optional `--install` validates
the default environment on the current host; it does not install every target
platform. Lock resolution covers the platforms configured in the project.
Resolution may install temporary solve/build environments inside the disposable
workspace, including when `--install` is absent. This is required for some PyPI
source packages; it does not install into your original checkout. `--install`
adds an explicit locked installation check of the default host environment.

Pixi suggestions are limited to pins and upper bounds that exclude a newer final
release, with evidence (artifact URL and sha256): conda packages via `pixi search`
on the manifest's channels, PyPI packages via the JSON Simple API of pypi.org or
the manifest's `[pypi-options]` indexes (credentials are stripped and never
sent). A failed lookup keeps the suggestion as "not established". In
`pyproject.toml` targets this includes `[project]` dependencies, optional
dependencies and `[dependency-groups]` requirements; direct-URL requirements
have no version to suggest or accept.

To accept a suggestion, pass `--accept NAME` (repeatable; Python
`UpdateOptions(accept=[...])`). The declared requirement is rewritten in the same
style to the newest release the evidence shows it excludes (`==X` → `==Y`,
`<=X` → `<=Y`, `X.*`, `~=X.Y` and conda `=X` keep their precision); comments
and layout are preserved, as are extras, markers and parentheses around a
`[project]` requirement's specifier. Ranges, `<X`, `!=` and `|` alternatives are ambiguous
and need `--accept NAME=REQUIREMENT` — the first `=` separates the name, so an
exact PyPI pin is `--accept six===1.17.0`. The staged manifest is then
re-resolved, scanned if requested and previewed; the manifest and lock diffs
apply together, and the validation list records what was accepted and why.
Accepting when no newer release is excluded is an error (exit 2).

Evidence respects the manifest's `exclude-newer` release-age policy, as Pixi
does: durations (`14d`, `2w`, `12h`, `1y` = 365.25 days, `14 days`), dates
(end of that day, UTC) and RFC 3339 timestamps. Releases without an upload time
are not treated as eligible; unrecognised values leave availability "not
established".

GitHub Actions updates retain tag/SHA style and the current major release line.
`--upgrade` allows a new major. `GITHUB_TOKEN` or `GH_TOKEN` supplies optional
GitHub API authentication. Local actions and Docker references are left alone.
Remote references that cannot be resolved (branches such as `@main`, SHAs that
match no release, missing alias tags) stay unchanged and are listed with a
reason under `unresolved`. A trailing `# vX.Y.Z` comment equal to the old tag is
updated with the reference.

Candidate files are applied without resolving again. Input changes invalidate a
proposal. Shared output conflicts block preparation; independent failed targets
require `--allow-partial` for partial application. `depsmith recover
--root PATH` restores an interrupted application only when files still contain
old or proposed bytes. Apply and recover hold an operating-system lock on
`.depsmith/lock`, so concurrent operations on one workspace fail fast;
the lock is released automatically if a process dies. The recovery journal is
published atomically (written, synced, then hard-linked into place), so a crash
never leaves a partial journal; this needs a filesystem with hard links.
Repository symlinks and dependencies outside the staged root are currently
rejected.

Backends run with a timeout (`timeout_seconds`) and, on Unix, in their own
process group, so a timeout or interrupt kills the backend together with the
processes it started (Windows uses `taskkill /T`, not yet exercised in CI).
Interrupting the CLI exits with status 130. Failures include the last 40 lines
of the backend's stderr with credentials redacted: URL passwords, token-like
parameters, authorization headers, GitHub tokens and values of secret-named
environment variables. Stdout is never echoed.

For Git repository roots and ordinary linked worktrees, staging preserves Git
history, tags, the index, and dirty source files so SCM-based package versions
can be calculated normally. Hooks and remote configuration are omitted. Git
metadata participates in stale-proposal detection; large histories add copying
and hashing cost. Select the repository root, not a subdirectory. Nested Git
repositories, submodules, external object stores, sparse checkouts, external
attributes, configuration includes/filters, and worktree-specific configuration
are currently rejected with a configuration error. Repository-local Git
environment overrides are cleared for backend processes; authentication helpers
such as `GIT_ASKPASS` and `GIT_SSH_COMMAND` remain available.

## Python

```python
from depsmith import UpdateOptions, discover, prepare

root = "/path/to/project"
print(discover(root))
proposal = prepare(root, targets=["pixi:pixi.toml"], options=UpdateOptions())
for change in proposal.changes:
    print(change.diff)
if not proposal.failures:
    result = proposal.apply()  # Applies the candidate held by this object.
```

`discover`, `prepare`, `scan`, `doctor`, and `recover` share the Rust core with the
CLI. Calls are synchronous and release the GIL during native work. Results use
typed dataclasses; invalid operations raise typed exceptions. `to_dict()` returns
a serializable report. Reports cannot be reloaded as executable proposals:
prepare and apply in the same process.

## CI and repository configuration

Save default targets and options in `depsmith.toml`:

```toml
targets = ["pixi:pixi.toml", "github-actions:.github/workflows/ci.yml"]

[options]
timeout_seconds = 300
upgrade = false
```

Explicit API/CLI settings override repository settings. Native Pixi project
configuration is staged even when `.pixi/` is ignored. The common cooldown flag
currently returns an unsupported error; configure Pixi's native policy instead.

A provider-neutral CI job, after installing the package and native tools:

```sh
depsmith update --root "$PROJECT_DIR" --apply --yes --non-interactive --json > "$REPORT_DIR/update.json"
```

Keep reports outside the project during preparation/application so creating a
report does not change the fingerprinted inputs. Use `--markdown` instead of
`--json` for a readable report with diffs. Committing and opening a PR belong to
your CI pipeline.

| Exit | Meaning |
|---|---|
| 0 | Success; check found no changes |
| 1 | Check found pending changes |
| 2 | Invalid arguments, configuration, or selection |
| 3 | Backend, validation, scanner, or application failure |
| 4 | Vulnerability policy rejection |
| 5 | Partial success |

## Optional vulnerability scanning

```sh
depsmith scan --root /path/to/project --target pixi:pixi.toml --fail-on high --json
depsmith check --root /path/to/project --target pixi:pixi.toml --scan --fail-on high --only-new --json
```

Grype scans baseline and candidate with one database snapshot per target.
Each report lists every candidate finding and, when a baseline lock exists
(`comparison = "baseline"`), classifies them as introduced, resolved or
remaining. Without one the report is `candidate-only`: nothing is claimed as
introduced or resolved, and `--only-new` assesses all findings because "new"
cannot be established. A scanner failure blocks that target's proposal (exit 3).
Each finding records its `artifact` and `applicability`: `upstream`, or unknown
build/backport status for conda packages matched through an upstream identity.

Automatic identity is limited to public PyPI artifacts from
`files.pythonhosted.org` and exact GitHub action release tags. Conda names are
never assumed to be PyPI names. Reviewed `[[options.identity_mappings]]` entries
can provide `ecosystem`, `name`, an unversioned `purl`, and HTTPS `evidence`.
The caller must review that evidence; the updater does not verify the linked
page. Unmapped packages, local packages, Git references, floating action tags,
and unknown versions can remain unassessed. Upstream matches do not establish
whether a conda build contains a backported fix. Policy success does not imply
complete coverage. SHA-pinned actions stay unassessed even with a version
comment, since the comment is not evidence.

Suppressions must name the advisory, a documented reason and a scope (a
package as `ecosystem:name`, a target, or both); an optional `expires` date is
the last day it applies. Suppressed findings stay in reports with the
suppression attached and are only excluded from `--fail-on` gates; expired
suppressions are not applied and are listed.

```toml
[[options.suppressions]]
id = "GHSA-v845-jxx5-vc9f"
package = "pypi:urllib3"
reason = "No cross-origin redirects with cookies in our usage"
expires = "2026-12-31"
```

## Development and release status

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python -m unittest discover -s tests/python -v
# Explicit integration test; downloads a Grype vulnerability database:
GRYPE=/path/to/grype cargo test -p depsmith-core --test grype_live -- --ignored
```

Python tests require the built package installed in the active interpreter.
Set `PIXI=/path/to/pixi` to enable the native Python update/apply test.

[CI](.github/workflows/ci.yml) builds and tests CLI artifacts and wheels for
Linux/Windows x86-64 and macOS Intel/ARM, checks Rust 1.89, installs from the
sdist, and builds/tests the local Linux conda package. The initial platform
matrix passed; native integration tests remain opt-in. See the
[release guide](docs/releasing.md) for validation scope, manual publication
setup, and GitHub Actions/provider-neutral update-job examples.

The [local conda recipe](recipes/depsmith/README.md) is a starting point for
conda-forge packaging, not a published feedstock. See the [design plan](docs/plan.md).
Remaining first-release work includes Windows process-tree acceptance and
conda-forge release prerequisites. Interactive suggestion prompts are deferred.
