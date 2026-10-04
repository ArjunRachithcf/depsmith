# Releasing

The project is an alpha. A green build is not a published release.

## Validation

CI builds standalone executables and installs/tests wheels on Linux x86-64,
Windows x86-64, macOS Intel and Apple Silicon. It checks Rust 1.89, installs
from the sdist, and builds/tests the conda-forge recipe on Linux with the
commit's own sdist. Native Pixi, uv, npm, cargo, conda-lock, Grype,
conda-forge, pypi.org, crates.io and GitHub API tests run in the separate Integration workflow on
pull requests (informational), on `main`, nightly (failures open a
`nightly-integration` issue) and as a gate in the Release workflow.
Windows process-tree termination still needs dedicated native acceptance coverage.

The initial complete platform run passed at
[`02ef062`](https://github.com/ArjunRachithcf/depsmith/actions/runs/36527117086).
The sdist-install and conda CI jobs were added subsequently and need their own run.
`recipes/depsmith/recipe.yaml` is the conda-forge recipe (rattler-build) that a
staged-recipes submission or the feedstock carries: it builds the latest
release's PyPI sdist, checked against its SHA-256, and ships the bundled
crates' licenses in `THIRDPARTY.yml`. While `recipes/depsmith/variants.yaml`
exists, packages go to the `conda-forge/label/depsmith_rc` channel; remove it
for a final release.

## Manual release workflow

1. Finish release acceptance and set the one version: `version` in
   `[workspace.package]` of `Cargo.toml` (and the two internal crate versions
   beside it). It is SemVer, `X.Y.Z` or a dotted pre-release
   `X.Y.Z-alpha.N` / `-beta.N` / `-rc.N` (the dot makes `rc.10` sort after
   `rc.9`). Python derives its own: maturin turns `0.1.0-rc.2` into
   `0.1.0rc2`, and the conda-forge recipe follows that PyPI release. Create the
   tag `v{version}` (`v0.1.0`, `v0.1.0-rc.2`) through the normal review
   process; `cargo binstall` downloads from `releases/download/v{version}`.
2. Run **Release** (`.github/workflows/release.yml`) with that existing tag and
   leave `publish` false. It resolves the tag to a commit, checks the version scheme
   (tag = v{Cargo version}; Python derived from it; a final release fails while
   `recipes/depsmith/variants.yaml` still targets the rc label), and runs all CI build/test jobs and the native integration tests
   (real Pixi, uv, npm, cargo, conda-lock, Grype, conda-forge, pypi.org, crates.io
   and GitHub API) on that exact commit.
3. Review the artifacts. Configure GitHub environments `pypi`, `crates` and
   `release` with required reviewers before enabling publication. crates.io
   [trusted publishers](https://crates.io/docs/trusted-publishing) can only be
   added to crates that exist, so the first publish uses a crates.io API token
   (scope `publish-new`) stored as the `CARGO_REGISTRY_TOKEN` secret of the
   `crates` environment. Afterwards add a trusted publisher to both
   `depsmith-core` and `depsmith` (repository `ArjunRachithcf/depsmith`,
   workflow `release.yml`, environment `crates`) and delete the secret; the
   workflow then uses trusted publishing. Publishing skips crate versions that
   already exist, so a re-run after a partial failure is safe. Configure a PyPI
   [Trusted Publisher](https://docs.pypi.org/trusted-publishers/adding-a-publisher/)
   for owner `ArjunRachithcf`, repository `depsmith`, workflow `release.yml`,
   environment `pypi`. Confirm ownership/availability of the PyPI project first.
4. Run **Release** again with `publish` true. Only after all build/test jobs
   succeed does it request the environment approvals. It uploads those tested
   wheels and sdist to PyPI, publishes `depsmith-core` then `depsmith` to
   crates.io, and creates a **draft** GitHub release (a pre-release for
   `a`/`b`/`rc` tags) with the CLI archives named by target triple, the install
   scripts, Python distributions, the Linux conda package and `SHA256SUMS`.
   Inspect and publish the GitHub draft separately.

PyPI publishing and draft creation are independent jobs: one may succeed while
another fails. Inspect their results before retrying; an existing PyPI version
cannot be replaced. Do not move a tag during a release. No conda upload is automated.
The GitHub environments and PyPI publisher are external setup, not created by
these workflow files. See [PyPI's publishing guidance](https://docs.pypi.org/trusted-publishers/using-a-publisher/).

## Update-job examples

- [GitHub Actions check](https://github.com/ArjunRachithcf/depsmith/blob/main/docs/examples/github-actions.yml): copy to a consuming
  repository after the pinned package version is published. It treats exit 1
  as available updates and preserves other errors. Reports are outside the
  fingerprinted checkout. Full Git history supports SCM-derived local versions.
- [Provider-neutral shell job](https://github.com/ArjunRachithcf/depsmith/blob/main/docs/examples/update.sh): install the package and
  the native tools your targets use first, then run `sh update.sh /path/to/project /outside/reports check`.
  Exit 1 means pending changes. Use `apply` for an explicit noninteractive update
  in one process; your pipeline owns committing and opening a PR afterwards.

Both examples select all discovered targets. Replace `--all` with explicit
`--target` flags for narrower scope. Scanning is opt-in: install Grype and add
`--scan` and the desired policy flags when required.
