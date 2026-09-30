# Configuration

Repository defaults live in `depsmith.toml` at the repository root:

```toml
targets = ["pixi:pixi.toml", "github-actions:.github/workflows/ci.yml"]

[options]
timeout_seconds = 300
upgrade = false
```

- `targets` is the saved selection, used when no `--target`/`--all` (CLI) or
  `targets=` (Python) is given.
- `[options]` accepts the fields of `UpdateOptions` (see the
  [Python API](Python-API)); unknown fields are an error.
- Explicit CLI flags and Python options override the file.
- Native package-manager configuration stays authoritative for ecosystem
  behaviour. Native Pixi configuration in `.pixi/config.toml` is staged even
  when `.pixi/` is ignored.

## Native tool paths

Each adapter declares the native tools it runs; `depsmith doctor` lists them
by name. Point one at a specific executable with `[options.tools]`, the
repeatable `--tool NAME=PATH` flag, or `UpdateOptions(tools={...})` in Python:

```toml
[options.tools]
pixi = "/opt/pixi/bin/pixi"
grype = "/usr/local/bin/grype"
conda = "/opt/micromamba/bin/micromamba"  # the solver conda-lock drives
```

`--pixi` and `--grype` (and the matching option fields) still work as
deprecated aliases.

## Saving an interactive selection

When no targets are configured, an interactive CLI run asks about each
discovered target, then offers to save the selection as `targets` (default
no). The rest of the file and its comments are kept, and an existing selection
is never replaced. JSON output, `--non-interactive` and the Python API never
prompt or save.

## Cooldown

`--cooldown-days` asks for a minimum release age. An adapter that cannot
enforce it rejects the request (exit 2) rather than ignoring it, because
ignoring it would relax your policy. For Pixi, set the native release-age
cutoff (`exclude-newer`) in the manifest instead; depsmith respects it (see
[Pixi adapter](Pixi-adapter)).

## Scanning options

Identity mappings and suppressions are configured as
`[[options.identity_mappings]]` and `[[options.suppressions]]`; see
[Vulnerability scanning](Vulnerability-scanning).
