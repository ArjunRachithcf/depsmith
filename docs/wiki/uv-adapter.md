# uv adapter

## Targets

Each `pyproject.toml` that owns a `uv.lock` is a target, such as
`uv:pyproject.toml`: a project with a `uv.lock` beside it or a `[tool.uv]`
table, that no enclosing workspace lists as a member. Members of a
`[tool.uv.workspace]` (its `members` globs, minus `exclude`) are not targets
of their own; their manifests are part of the workspace root's target. A
`pyproject.toml` with a `[tool.pixi]` table belongs to the
[Pixi adapter](Pixi-adapter), and a plain `pyproject.toml` (Poetry, Hatch,
PDM) is not claimed. `uv.lock`, `uv.toml` and `.python-version` are staged
even when ignored by Git, and `.venv/` is never walked.

## Updates

An **update** runs `uv lock` inside the stage and keeps every declared
requirement; `uv.lock` covers every platform.

- With no Git dependencies locked, every package is upgraded
  (`uv lock --upgrade`).
- **Git dependencies** stay at their locked commit unless `--refresh-git` is
  given: every other locked package is upgraded by name
  (`--upgrade-package`) instead.
- `--package NAME` (repeatable) upgrades only the named direct dependencies.
  Names match by PEP 503 normalisation.
- The candidate is checked with `uv lock --locked`, and the manifests must not
  change.
- `--upgrade`, `--cooldown-days` and `--install` are not supported. Change a
  requirement with `--accept`; for a release-age cutoff, set
  `exclude-newer` in `[tool.uv]` or `uv.toml`.

Settings such as `UV_INDEX_URL` in the environment reach uv unchanged, as
they would for a plain `uv lock`.

## Suggestions

Declarations are the PEP 508 requirements in `[project.dependencies]`,
`[project.optional-dependencies]`, `[dependency-groups]` and the legacy
`tool.uv.dev-dependencies` of the root and member manifests. Packages with a
`[tool.uv.sources]` entry (Git, path, URL, workspace member, or pinned to a
named index) are not looked up; the workspace root's sources apply to every
member.

Evidence comes from the project's indexes through the PEP 691 JSON API, in
uv's priority order: `[[tool.uv.index]]` entries (except `explicit` ones),
then `extra-index-url`, then the default (an index marked `default = true`,
`index-url`, or PyPI). A `uv.toml` replaces `[tool.uv]` settings, as in uv.
Credentials in index URLs are never sent. `exclude-newer` timestamps and
durations apply to the evidence; a bare date, which uv reads in the local
time zone, leaves availability unestablished instead of guessing.

## Accepting a suggestion

`--accept NAME` rewrites the requirement as for PyPI requirements of the
[Pixi adapter](Pixi-adapter): in its own style, keeping extras, markers,
comments and layout, before uv resolves again. `--accept NAME=REQUIREMENT`
must be a valid PEP 440 specifier.

## Scanning

Packages from an index are inventoried with their sdist (else first wheel)
URL and scanned as `pkg:pypi/NAME` when the artifact comes from PyPI. Git
packages keep their locked commit; editable, virtual and path packages are
the project itself and are not inventoried.
