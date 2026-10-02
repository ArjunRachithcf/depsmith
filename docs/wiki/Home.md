# depsmith

depsmith prepares, reviews and applies dependency updates for a repository. One
Rust engine is shared by a command-line tool and a typed Python API. Each target
is resolved by its native package manager inside a **stage** (a disposable copy
of the repository), so you review exact file and dependency changes before
anything is written, and the reviewed files are applied without resolving again.

Supported today: **Pixi** (`pixi.toml` and Pixi-managed `pyproject.toml`),
**GitHub Actions** workflow references, **Cargo** (`Cargo.lock` owners),
**conda** (`environment.yml` locked with conda-lock) and **uv**
(`pyproject.toml` locked with `uv.lock`). `depsmith init` checks the native
tools your targets use and can install the missing ones.

## Guide

- [Getting started](Getting-started): install, check a project, apply updates.
- [Configuration](Configuration): `depsmith.toml`, saved targets and options.
- [Reviewing and applying](Reviewing-and-applying): proposals, stale inputs, partial application and recovery.
- [Pixi adapter](Pixi-adapter): updates, upgrades, suggestions and acceptance.
- [GitHub Actions adapter](GitHub-Actions-adapter): release lines, commit pins and unresolved references.
- [Cargo adapter](Cargo-adapter): lock owners, MSRV-aware updates, suggestions and acceptance.
- [Conda adapter](Conda-adapter): conda-lock, sharded repodata evidence and `pip:` requirements.
- [uv adapter](uv-adapter): uv workspaces, Git pins, index evidence and acceptance.
- [Vulnerability scanning](Vulnerability-scanning): baseline comparison, identities, policy and suppressions.
- [CI integration](CI-integration): noninteractive jobs, reports and exit statuses.
- [Troubleshooting](Troubleshooting): common errors and what to do.

## Reference

- [CLI reference](CLI-reference) and [Python API](Python-API), generated from the code.

## Contributing

- [Architecture](Architecture), [Development and testing](Development-and-testing), [Releasing](Releasing).

Terms follow the project glossary,
[`CONTEXT.md`](https://github.com/ArjunRachithcf/depsmith/blob/main/CONTEXT.md).
