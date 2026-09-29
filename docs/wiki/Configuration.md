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
