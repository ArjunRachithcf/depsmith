# Pixi adapter

## Targets

`pixi.toml` files and `pyproject.toml` files with a `[tool.pixi]` table are
targets (`pixi:pixi.toml`, `pixi:pyproject.toml`). A Python project without
`[tool.pixi]` is a uv target when it has a `[tool.uv]` table or a `uv.lock`
(see _uv projects_ below), and otherwise not a target.

## Updates and upgrades

An **update** re-resolves the lock within the declared constraints, for every
configured platform, and keeps platform targets, features, environments,
channels, local paths and intentional pins. A failure on any platform blocks
that target's proposal.

- `--package NAME` (repeatable) updates only the named **direct dependencies**:
  names in Pixi dependency tables, and for `pyproject.toml` also `[project]`
  dependencies, optional dependencies and dependency groups. Unknown names fail
  with exit 2 before Pixi runs; targets that declare none of the names are
  skipped. PyPI names match after PEP 503 normalisation; conda names match
  case-insensitively only.
- `--upgrade --package NAME` is an **upgrade**: it lets Pixi change those
  packages' declared constraints.
- **Git pins** stay at their locked commit unless `--refresh-git` is given.
- `--install` additionally installs the candidate's default environment on the
  current host; it does not install every platform.

Resolution can create temporary solve and build environments inside the stage,
even without `--install`; some PyPI source packages need this. Pixi builds
editable and other source PyPI packages for the machine running the update, so
such projects must list that machine's platform (for example `osx-arm64` on
Apple Silicon) in `platforms`; otherwise Pixi's error is reported with its
hint.

## Suggestions

Suggestions report pins and upper bounds that exclude a newer final release.
Each names the affected declaration and cites **evidence** (artifact URL and
SHA-256):

- conda packages: `pixi search` on the manifest's channels;
- PyPI packages: the JSON Simple API of pypi.org, or the manifest's
  `[pypi-options]` indexes. Credentials in index URLs are stripped and never
  sent.

Lower bounds alone never produce suggestions. A failed lookup keeps the
suggestion marked "not established". In `pyproject.toml` targets this covers
`[project]` dependencies, optional dependencies and dependency groups;
direct-URL requirements have no version to suggest.

Evidence respects the manifest's `exclude-newer` release-age cutoff the way
Pixi does: durations (`14d`, `2w`, `12h`, `1y` = 365.25 days, `14 days`), dates
(end of that day, UTC) and RFC 3339 timestamps. Releases without an upload
time are not treated as eligible, and unrecognised values leave availability
"not established".

## Accepting a suggestion

`--accept NAME` (repeatable; Python `UpdateOptions(accept=[...])`) rewrites the
declared requirement in the same style to the newest release the evidence
shows it excludes: `==X` → `==Y`, `<=X` → `<=Y`, and `X.*`, `~=X.Y` and conda
`=X` keep their precision. Comments and layout are kept, as are extras,
markers and parentheses around a `[project]` requirement's specifier.

Ranges, `<X`, `!=` and `|` alternatives are ambiguous and need
`--accept NAME=REQUIREMENT`. The first `=` separates the name, so an exact PyPI
pin is `--accept six===1.17.0`. The staged manifest is then resolved, scanned if
requested and previewed; manifest and lock apply together, and the validation
list records what was accepted and why. Accepting when no newer release is
excluded is an error (exit 2).

## uv projects

A `pyproject.toml` that owns a `uv.lock` is a uv target (`uv:pyproject.toml`):
it has a `[tool.uv]` table or a `uv.lock` beside it, no `[tool.pixi]` table,
and is not a member of an enclosing uv workspace. Members listed in
`[tool.uv.workspace]` (`members` globs, minus `exclude`) belong to the root's
target. `uv.toml` and `.python-version` are staged with the lock; `.venv` is
never walked.

- An **update** runs `uv lock --upgrade` in the stage within the declared
  requirements; `--package NAME` passes `--upgrade-package NAME` instead.
  Names match after PEP 503 normalisation. Git pins stay unless
  `--refresh-git` is given. The candidate is checked with `uv lock --locked`,
  and manifests must not change.
- `--upgrade` and `--install` are not supported, and `--cooldown-days` is
  rejected: set uv's `exclude-newer` instead.
- **Suggestions** cover `[project]` dependencies, optional dependencies,
  `[dependency-groups]` and `tool.uv.dev-dependencies` of the root and member
  manifests, except packages with a `[tool.uv.sources]` entry. Evidence comes
  from the project's indexes in uv's order (`[[tool.uv.index]]` entries other
  than `explicit` ones, `extra-index-url`, then `index-url` or pypi.org), read
  from `uv.toml` instead of `[tool.uv]` when it exists. `exclude-newer` is
  respected as above, except that a bare date leaves availability "not
  established", because uv reads it in the local time zone.
- `--accept` rewrites requirements as for PyPI requirements above.
