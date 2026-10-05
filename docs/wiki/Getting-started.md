# Getting started

## Install

depsmith needs Python 3.10+ (for the package) or Rust 1.89+ (to build from
source). Each target is resolved by its native package manager (Pixi, uv,
cargo, conda-lock and micromamba, npm), and Grype scans for vulnerabilities.

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

From a checkout: `python -m pip install .`, or
`cargo build --release --locked -p depsmith` for the executable alone.

```sh
depsmith --version
depsmith init --root /path/to/project   # choose targets, save them, check their tools
depsmith doctor                  # capabilities and every native tool's status
```

`depsmith init` sets a project up. It discovers the targets and asks which to
work on, a package manager at a time (then file by file when you decline a
group), and saves the choice as `targets` in `depsmith.toml`, which `check`,
`update` and `scan` then use. Run it again later and it keeps the saved
targets and offers only the ones found since; without a terminal it chooses
nothing and lists them as not selected. `--all` or `--target` choose without
asking, and `--no-save` leaves `depsmith.toml` alone. It warns when a target
declares depsmith itself as a dependency, directly or in a local project it
installs (such as a Pixi `{ path = "." }` package): depsmith belongs in a tool
environment (`uv tool install depsmith`, or `uvx depsmith`), not in the
project it updates, where it is resolved with the project's own constraints.
It then reports what the chosen targets' adapters support.

It also checks only the native tools the targets use
(and Grype when `scan` is configured), and offers to install each missing one:
the release depsmith is tested with, downloaded over HTTPS and checked against
its pinned sha256, into a per-user tool cache (`DEPSMITH_TOOLS_DIR` overrides
it). Each question says why the tool is offered (not installed, changed,
outdated, or failing to run). Answer per tool, or pass `--fetch-tools` to
install without asking (in CI); in Python, `depsmith.init(root,
fetch_tools=True)` never prompts. A failed install is reported and the others continue; `init` exits
3 while a used tool is still missing. Each
install is recorded with its sha256 in the project's `.depsmith/` directory
(which ignores itself in Git); a cached tool that changed, or that depsmith
no longer pins, is not used until `init` installs it again. cargo comes with the
tested Rust toolchain (a pinned rustup, kept inside the cache), conda-lock
is installed with uv from hash-locked requirements (uv is offered first when
missing), and npm with a pinned Node.js LTS. Tools on `PATH` or given
with `--tool NAME=PATH` are always used as they are.

A standalone executable builds with `cargo build --release --locked -p depsmith`.

`depsmith doctor` reports each native tool as `tested`, `untested` or
`unavailable`, with where it was found (`configured`, `path`, `downloaded`,
`untrusted` or `missing`). Other versions than the tested ones are untested,
not assumed incompatible. It also lists what each
adapter supports.

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
