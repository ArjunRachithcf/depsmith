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
  (`environment.yml` locked with conda-lock) and uv (`pyproject.toml` locked
  with `uv.lock`).
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
python -m pip install .   # from a checkout; Python 3.10+
depsmith doctor           # checks for the native tools (Pixi, cargo, conda-lock, uv, Grype)
depsmith init             # offers to install the missing tools your targets use
```

A standalone executable builds with `cargo build --release --locked -p depsmith-cli`
(Rust 1.89+). See [Getting started](https://github.com/ArjunRachithcf/depsmith/wiki/Getting-started).

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
| uv projects and workspaces, project indexes | [Pixi adapter](https://github.com/ArjunRachithcf/depsmith/wiki/Pixi-adapter) |
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
