# Getting started

## Install

depsmith needs Python 3.10+ (for the package) or Rust 1.89+ (to build from
source). The native package managers are installed separately (Pixi for Pixi
targets, uv for uv targets, and Grype if you scan for vulnerabilities), or
by `depsmith init` (see below).

```sh
python -m pip install .          # from a checkout
depsmith --version
depsmith doctor                  # capabilities and native tool status
```

A standalone executable builds with `cargo build --release --locked -p depsmith-cli`.

`depsmith doctor` reports each native tool as `tested`, `untested` or
`unavailable`, and lists the versions that have been exercised (such as Pixi
0.80.0, uv 0.12.15 and Grype 0.119.0); other versions are untested, not
assumed incompatible. It also lists what each adapter supports.

## Set up native tools

```sh
depsmith init --root /path/to/project
```

`init` checks the native tools the discovered targets use (`--target` narrows
them; `--scan` adds Grype). For each missing one that has a pinned release for
this host (Pixi, uv, Grype, and micromamba as the conda solver), it asks
before downloading that release, checks its SHA-256 and installs it into the
tool cache: `DEPSMITH_TOOLS_DIR`, or `depsmith/tools` under the user's cache
directory. `--fetch-tools` installs without asking; noninteractive runs
otherwise install nothing. cargo and conda-lock are never installed.

A tool on `PATH` or given with `--tool` is used first. A cached tool is used
only by repositories that `init` installed it for, and only while it is
unchanged: the install is recorded in `.depsmith/tools.json`, which Git
ignores. `init` exits 3 when a used tool is still missing. From Python, call
`init(root, fetch_tools=True)`.

## Check a project

```sh
depsmith discover --root /path/to/project
depsmith check --root /path/to/project --target pixi:pixi.toml
```

`discover` lists **targets** by identifier, such as `pixi:pixi.toml`,
`pixi:pyproject.toml` or `github-actions:.github/workflows/ci.yml`. Select them
with repeated `--target` flags or `--all`; interactive runs can also choose and
save them (see [Configuration](Configuration)).

`check` prepares a **proposal** in a stage and writes nothing. It exits 1 when
updates are pending and 0 when there are none. Add `--json` or `--markdown` for
machine-readable or review-friendly reports.

## Apply updates

```sh
depsmith update --root /path/to/project --target pixi:pixi.toml
```

Interactively, `update` shows the proposal and diffs and asks before applying.
In CI use `--apply --yes --non-interactive`. The reviewed files are written
exactly as previewed; see [Reviewing and applying](Reviewing-and-applying).

Updates keep your declared constraints. To move a constraint, upgrade selected
direct dependencies with `--upgrade --package NAME`, or accept an
evidence-backed suggestion with `--accept NAME` (see [Pixi adapter](Pixi-adapter)).

## From Python

```python
from depsmith import UpdateOptions, discover, prepare

root = "/path/to/project"
print(discover(root))
proposal = prepare(root, targets=["pixi:pixi.toml"], options=UpdateOptions())
for change in proposal.changes:
    print(change.diff)
if not proposal.failures:
    proposal.apply()  # applies the candidate held by this object
```

Calls are synchronous, release the GIL during native work, never prompt, and
raise typed exceptions. A proposal must be applied in the process that prepared
it; `to_dict()` gives a report, not a reloadable proposal. See
[Python API](Python-API).
