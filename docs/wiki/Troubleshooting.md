# Troubleshooting

Start with `depsmith doctor`: it shows whether Pixi and Grype are found, which
versions have been tested, and what each adapter supports.

| Message or symptom | Cause and fix |
|---|---|
| Exit 2, "not supported by the … adapter" / "cannot be enforced by the … adapter" | The requested option is unsupported for that target. Remove it, or follow the hint. `--cooldown-days` is rejected rather than ignored; use Pixi's `exclude-newer`. Other options that do not apply to a target (for example `--install` on a workflow) are only noted under validation. |
| Exit 2, "not a direct dependency of any selected target" | `--package`/`--accept` names must be declared directly by a selected target. Check spelling; conda names are case-insensitive but distinguish `-` and `_`. |
| "proposal is stale" | The repository changed after preparing (files, tags or the index). Prepare again. |
| "another depsmith operation is in progress in this repository" | Another apply or recover holds `.depsmith/lock`. Wait for it; the lock is released automatically if that process died. |
| "no pixi.lock to scan" | `scan` needs an existing lock. Create it with `pixi lock` or `depsmith update`. |
| "The workspace does not support '<platform>' on this machine" | Editable or source PyPI packages are built for the machine running the update; add its platform to Pixi's `platforms`. |
| "the selected root is inside a Git repository" | Select the repository root, not a subdirectory. |
| Git layout rejected (submodules, sparse checkout, includes, …) | These layouts cannot be staged reproducibly yet. |
| "local dependency escapes the repository" | Local path dependencies must be inside the repository. |
| Exit 3 with a backend's error | The last 40 stderr lines are shown with credentials redacted. Timeouts: raise `--timeout-seconds`. |
| Exit 5 | Some targets failed; others can be applied with `--allow-partial`. |
| An apply was interrupted | Run `depsmith recover --root PATH`. |
| Suggestion "not established" | The availability lookup failed (network, authenticated or non-JSON index, unrecognised `exclude-newer`). The suggestion is kept, not dropped. |
| Package listed as unassessed | No verifiable identity; add a reviewed identity mapping if appropriate. Unassessed is never clean. |
