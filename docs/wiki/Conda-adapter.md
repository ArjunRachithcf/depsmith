# Conda adapter

## Targets

`environment.yml` and `environment.yaml` files with a `dependencies:` list are
targets (`conda:environment.yml`), locked with
[conda-lock](https://github.com/conda/conda-lock). The lock is
`<name>.conda-lock.yml` when it exists next to the environment file (so
`environment.conda-lock.yml` is recognised), otherwise conda-lock's default
`conda-lock.yml`.

conda-lock needs a solver: point the `conda` tool at conda, mamba or
micromamba (`--tool conda=/path/to/micromamba`; micromamba is detected by
name). `depsmith doctor` lists both tools.

## Updates

An **update** runs `conda-lock lock` inside the stage for the platforms in the
environment's `platforms:` list, or conda-lock's defaults. Without
`--package` every package is re-solved; `--package NAME` (repeatable) passes
`--update NAME` so only those packages move.

The candidate lock must hold every declared package on every locked platform,
and the environment file must not change. `--upgrade` is not supported:
change a requirement with `--accept`.

## Suggestions

Declarations are the conda match specs in `dependencies:` (channel prefixes
such as `conda-forge::six` are allowed) and the requirements in the `pip:`
sub-list, which belong to PyPI. Pins and upper bounds that exclude a newer
final release are suggested with **evidence**:

- conda packages: **sharded repodata** (CEP 16) from the environment's
  channels, or `repodata.json` for channels without shards. Each platform is
  compared with its own subdir plus `noarch`, as a solver sees it.
- PyPI packages: the JSON Simple API of pypi.org.

Specifications depsmith cannot evaluate (regular expressions, bracket forms,
build strings) leave availability "not established" instead of guessing.

## Accepting a suggestion

`--accept NAME` rewrites the requirement in the same style, as for the
[Pixi adapter](Pixi-adapter): `==X` → `==Y`, `X.*` and conda `=X` keep their
precision, and ranges need `--accept NAME=REQUIREMENT`. Only the list item's
line changes; quotes and comments are kept.

## Dependency introducers

A changed package that the environment does not declare is reported with the
declared dependencies that pull it in. The graph comes from the conda-lock
lock, which records each package's `dependencies` with their requirements,
per platform; virtual packages such as `__glibc` are left out.

## Scanning

Conda packages are scanned through reviewed identity mappings, and PyPI
packages from files.pythonhosted.org have their own identity; see
[Vulnerability scanning](Vulnerability-scanning).
