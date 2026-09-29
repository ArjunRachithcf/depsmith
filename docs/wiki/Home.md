# depsmith

depsmith prepares, reviews and applies dependency updates for a repository. One
Rust engine is shared by a command-line tool and a typed Python API. Each target
is resolved by its native package manager inside a **stage** (a disposable copy
of the repository), so you review exact file and dependency changes before
anything is written, and the reviewed files are applied without resolving again.

Supported today: **Pixi** (`pixi.toml` and Pixi-managed `pyproject.toml`) and
**GitHub Actions** workflow references. Conda-family and uv adapters follow the
first release.

## Guide

- [Getting started](Getting-started): install, check a project, apply updates.
- [Configuration](Configuration): `depsmith.toml`, saved targets and options.
- [Reviewing and applying](Reviewing-and-applying): proposals, stale inputs, partial application and recovery.
- [Pixi adapter](Pixi-adapter): updates, upgrades, suggestions and acceptance.
- [GitHub Actions adapter](GitHub-Actions-adapter): release lines, pins and unresolved references.
- [Vulnerability scanning](Vulnerability-scanning): baseline comparison, identities, policy and suppressions.
- [CI integration](CI-integration): noninteractive jobs, reports and exit statuses.
- [Troubleshooting](Troubleshooting): common errors and what to do.

## Reference

- [CLI reference](CLI-reference) and [Python API](Python-API), generated from the code.

## Contributing

- [Architecture](Architecture), [Development and testing](Development-and-testing), [Releasing](Releasing).

Terms follow the project glossary,
[`CONTEXT.md`](https://github.com/ArjunRachithcf/depsmith/blob/main/CONTEXT.md).
