# depsmith

[![CI](https://github.com/ArjunRachithcf/depsmith/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/ArjunRachithcf/depsmith/actions/workflows/ci.yml?query=branch%3Amain)
[![Coverage](https://codecov.io/gh/ArjunRachithcf/depsmith/graph/badge.svg?branch=main)](https://codecov.io/gh/ArjunRachithcf/depsmith)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Prepare, review and apply dependency updates for a repository, from a Rust CLI
or a typed Python API. Each target is resolved by its own package manager in a
disposable copy of the repository. You review the exact file and dependency
changes, and the reviewed files are then written without resolving again.

- **Supported:** Pixi (`pixi.toml`, Pixi-managed `pyproject.toml`), GitHub
  Actions workflow references, Cargo (`Cargo.lock` owners), conda
  (`environment.yml` locked with conda-lock), uv (`pyproject.toml` with
  `uv.lock`) and npm (`package.json` with `package-lock.json`).
- **Safe by default:** updates keep your constraints. Proposals go stale if the
  repository changes, and interrupted writes can be recovered.
- **Evidence-backed suggestions** for pins that block newer releases, which you
  can accept in the same declaration style.
- **Optional vulnerability scanning** compares before and after with one
  database snapshot. Packages it cannot assess are reported, never treated as
  clean.

📖 **Documentation: [the wiki](https://github.com/ArjunRachithcf/depsmith/wiki)**

## Install

```sh
pip install --pre depsmith         # Python 3.10+: CLI and Python API
cargo install depsmith             # build the CLI from crates.io (Rust 1.89+)
cargo binstall depsmith            # the release executable, via cargo-binstall
curl -fsSL https://github.com/ArjunRachithcf/depsmith/releases/latest/download/install.sh | sh
```

On Windows, `irm https://github.com/ArjunRachithcf/depsmith/releases/latest/download/install.ps1 | iex`.
The install scripts download the release executable for your platform, verify
it against the release's `SHA256SUMS` before installing anything, and install
it to `~/.local/bin` (`%LOCALAPPDATA%\depsmith\bin` on Windows); they never
edit your shell startup files. While only release candidates are published,
ask for one explicitly:

```sh
curl -fsSL https://github.com/ArjunRachithcf/depsmith/releases/download/v0.1.0-rc.2/install.sh | sh -s -- --version v0.1.0-rc.2
```

```powershell
$env:DEPSMITH_VERSION = "v0.1.0-rc.2"
irm https://github.com/ArjunRachithcf/depsmith/releases/download/v0.1.0-rc.2/install.ps1 | iex
```

Other options: `--pre` (newest release including pre-releases), `--prefix DIR`
(`$env:DEPSMITH_PREFIX`), and `--help`.

Then `depsmith init` asks which targets to work on, saves them to
`depsmith.toml`, and checks the native tools they use, offering to install the
missing ones. Install depsmith as a tool, not as a dependency of the project it
updates. See
[Getting started](https://github.com/ArjunRachithcf/depsmith/wiki/Getting-started).

## Quick start

```sh
depsmith discover --root path/to/project
depsmith check    --root path/to/project --target pixi:pixi.toml
depsmith update   --root path/to/project --target pixi:pixi.toml
```

`check` writes nothing and exits 1 when updates are pending. `update` shows
the changes and asks before applying. In CI, add `--apply --yes
--non-interactive --json`
([CI integration](https://github.com/ArjunRachithcf/depsmith/wiki/CI-integration)).

```python
from depsmith import UpdateOptions, prepare

proposal = prepare(
    "path/to/project", targets=["pixi:pixi.toml"], options=UpdateOptions()
)
for change in proposal.changes:
    print(change.diff)
if not proposal.failures:
    proposal.apply()
```

## Learn more

| Topic | Page |
|---|---|
| Saved targets and options (`depsmith.toml`) | [Configuration](https://github.com/ArjunRachithcf/depsmith/wiki/Configuration) |
| Proposals, stale inputs, partial apply, recovery | [Reviewing and applying](https://github.com/ArjunRachithcf/depsmith/wiki/Reviewing-and-applying) |
| Pixi updates, upgrades, suggestions, `--accept` | [Pixi adapter](https://github.com/ArjunRachithcf/depsmith/wiki/Pixi-adapter) |
| Actions release lines and commit pins | [GitHub Actions adapter](https://github.com/ArjunRachithcf/depsmith/wiki/GitHub-Actions-adapter) |
| Cargo lock owners, MSRV-aware updates | [Cargo adapter](https://github.com/ArjunRachithcf/depsmith/wiki/Cargo-adapter) |
| conda-lock environments, sharded repodata | [Conda adapter](https://github.com/ArjunRachithcf/depsmith/wiki/Conda-adapter) |
| uv workspaces, Git pins, index evidence | [uv adapter](https://github.com/ArjunRachithcf/depsmith/wiki/uv-adapter) |
| npm workspaces, cooldowns, registry evidence | [npm adapter](https://github.com/ArjunRachithcf/depsmith/wiki/npm-adapter) |
| Grype scanning, identities, policy, suppressions | [Vulnerability scanning](https://github.com/ArjunRachithcf/depsmith/wiki/Vulnerability-scanning) |
| Common errors | [Troubleshooting](https://github.com/ArjunRachithcf/depsmith/wiki/Troubleshooting) |
| Every command and option | [CLI reference](https://github.com/ArjunRachithcf/depsmith/wiki/CLI-reference) |
| The Python API | [Python API](https://github.com/ArjunRachithcf/depsmith/wiki/Python-API) |

The wiki is generated from [`docs/wiki/`](docs/wiki), and its terms follow the
glossary in [`CONTEXT.md`](CONTEXT.md).

## Status

Alpha. The first release will be published to PyPI and as GitHub release
binaries, with conda-forge after that
([Releasing](https://github.com/ArjunRachithcf/depsmith/wiki/Releasing)).
Post-install project checks and interactive
suggestion prompts are planned. New package managers plug in through one
adapter module and a shared conformance suite
([Architecture](https://github.com/ArjunRachithcf/depsmith/wiki/Architecture)).

## Contributing

See [Development and testing](https://github.com/ArjunRachithcf/depsmith/wiki/Development-and-testing)
and [Architecture](https://github.com/ArjunRachithcf/depsmith/wiki/Architecture).
Install the hooks with `prek install --hook-type pre-commit --hook-type pre-push`.
Changes land through pull requests with signed commits.

## License

[MIT](LICENSE)
