# Releasing

The project is an alpha. A green build is not a published release.

## Validation

CI builds standalone executables and installs/tests wheels on Linux x86-64,
Windows x86-64, macOS Intel and Apple Silicon. It checks Rust 1.89, installs
from the sdist, and builds/tests the local conda recipe on Linux. Native Pixi,
Grype, pypi.org and GitHub API tests run in the separate Integration workflow on
pull requests (informational), on `main`, nightly (failures open a
`nightly-integration` issue) and as a gate in the Release workflow.
Windows process-tree termination still needs dedicated native acceptance coverage.

The initial complete platform run passed at
[`02ef062`](https://github.com/ArjunRachithcf/depsmith/actions/runs/36527117086).
The sdist-install and conda CI jobs were added subsequently and need their own run.
Conda-forge submission also requires immutable release sources, their SHA-256,
vendored Rust dependencies and agreed feedstock maintainers. The local recipe
uses the checkout and downloads Cargo dependencies; it is not a feedstock.

## Manual release workflow

1. Finish release acceptance, keep Cargo, Python and conda recipe versions equal, and create
   the matching version tag (`v0.1.0`, for example) through the normal review process.
2. Run **Release** (`.github/workflows/release.yml`) with that existing tag and
   leave `publish` false. It resolves the tag to a commit, checks all three version
   declarations, and runs all CI build/test jobs and the native integration tests
   (real Pixi, Grype, conda-forge, pypi.org and GitHub API) on that exact commit.
3. Review the artifacts. Configure GitHub environments `pypi` and `release`
   with required reviewers before enabling publication. Configure a PyPI
   [Trusted Publisher](https://docs.pypi.org/trusted-publishers/adding-a-publisher/)
   for owner `ArjunRachithcf`, repository `depsmith`, workflow `release.yml`,
   environment `pypi`. Confirm ownership/availability of the PyPI project first.
4. Run **Release** again with `publish` true. Only after all build/test jobs
   succeed does it request the environment approvals. It uploads those tested
   wheels and sdist to PyPI and creates a **draft** GitHub release with CLI
   archives, Python distributions, the Linux conda package and SHA-256 checksums.
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
  Pixi first, then run `sh update.sh /path/to/project /outside/reports check`.
  Exit 1 means pending changes. Use `apply` for an explicit noninteractive update
  in one process; your pipeline owns committing and opening a PR afterwards.

Both examples select all discovered targets. Replace `--all` with explicit
`--target` flags for narrower scope. Scanning is opt-in: install Grype and add
`--scan` and the desired policy flags when required.
