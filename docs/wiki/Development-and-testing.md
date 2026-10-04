# Development and testing

## Setup

Rust 1.89+, Python 3.10+, and [prek](https://github.com/j178/prek) for the
repository hooks:

```sh
prek install --hook-type pre-commit --hook-type pre-push
```

Formatting, Ruff (including docstring rules), YAML/TOML and workflow lint run
on commit; Clippy runs on push.
`prek run --all-files` checks everything, as CI does.

## Tests

```sh
cargo test --workspace --locked                      # unit, integration, end-to-end, doctests
python -m pip install . && python -m unittest discover -s tests/python -v
```

Python tests need the built package installed. `python -m unittest discover
-s scripts/tests` covers the repository scripts, including the install
scripts against a local fake release (`install.ps1` too, when `pwsh` is
installed). Three layers:

1. **Offline end-to-end** (`crates/cli/tests/e2e.rs`): the real executable with
   a cross-platform Pixi stand-in on every CI host.
2. **Native integration** (ignored by default): real Pixi, uv, cargo,
   conda-lock, npm, Grype, pypi.org, crates.io, the npm registry and the
   GitHub API, plus installs of the pinned uv, Rust toolchain, conda-lock and
   Node.js.
   `PIXI=… UV=… CARGO=… CONDA_LOCK=… CONDA_SOLVER=… NPM=… GRYPE=… GITHUB_TOKEN=… cargo test --workspace -- --ignored`,
   and `PIXI=…` for the native Python tests.
3. **CLI/Python parity** (`tests/python/test_parity.py`).

## Documentation

Public Rust items and the public Python API must be documented (`missing_docs`,
Ruff `D`); `cargo doc` runs with warnings as errors. Follow the glossary in
`CONTEXT.md`. The [CLI reference](CLI-reference) and [Python API](Python-API)
pages are generated: run `python scripts/gen-reference.py` (with the package
installed) after changing help text or docstrings. CI's `docs-reference` job
fails when they are stale or a wiki link is broken.

## CI and merging

- **CI** builds and tests on Linux, Windows and macOS (Intel and Apple
  Silicon), checks Rust 1.89, the sdist, the conda recipe, the end-to-end tests
  and prek.
- **Distribution:** the `crates` job packages and verifies both crates as
  crates.io would (`cargo publish --workspace --dry-run`); `install-scripts`
  runs the install script tests on macOS and Windows.
- **Coverage:** Codecov requires 80% of changed lines covered, and the in-repo
  gate requires 75% total line coverage.
- **Integration** runs the native tests on pull requests (informational), on
  `main`, nightly (failures open a `nightly-integration` issue) and before
  releases.
- **Latest tools** runs weekly. A tool's candidate is its highest stable
  release published at least 7 days ago and newer than the tested one, so new
  releases settle first and nothing moves back. Integration runs once per
  candidate with only that tool changed. When it passes on every OS,
  `scripts/tool-drift.py` opens a pull request on `latest-tools/<tool>` that
  bumps the adapter's `tested_versions`, its pinned downloads in
  `provision.rs` (with the release's sha256 digests) and its pin in
  `.github/tool-versions.json` together; review and land it like any other.
  A candidate that breaks Integration, a pinned download that is gone or whose
  sha256 changed, or a bump that cannot be made opens a `latest-tools` issue
  instead (one per tool). cargo, which CI runs as stable Rust, is never bumped
  automatically, and npm (with its pinned Node.js) is not tracked yet.
  `crates/core/tests/pins.rs` checks the pins agree with the
  tested versions.
- Changes reach `main` through pull requests with signed commits; the required
  checks must pass.
