---
status: proposed
---

# Impact analysis follows a lock change through the target's own code to the tests that can break, and gives each finding a reach verdict

A proposal lists which packages move, but not what in the target's code uses them. A reviewer either runs every test or trusts a green run that never touched the changed package, and a vulnerable package that nothing imports looks as urgent as one on a request path. So depsmith analyses the **impact** of a lock change: the **affected packages**, the **usage sites** in first-party code, the tests that reach them through the **import graph**, the affected code no test reaches, and a static check of that code against the new environment.

`depsmith affected` reports the impact of a lock change, and `depsmith test` runs the tests it selects (`--dry-run` prints the commands). `check --impact` and `update --impact` attach the impact of the stage's lock change to the proposal. The same analysis gives each finding a **reach verdict**, which ADR 0004's security updates use. The engine lives in depsmith-core, so the CLI, the Python API and security updates share it.

## Consequences

- **The comparison** is between two locks of the target. The adapter's inventory decides what changed, since not every lock graph records versions or sources. Its lock graph (ADR 0005) decides what the change affects. A side without the target's lock is an empty lock, so locking for the first time adds every package.
  - The "before" side comes from git (`--base REF`, the merge-base by default when `CI` is set) or a file (`--old-lockfile`). The "after" side is the worktree, the index (`--staged`, the default locally), or a revision checked out in a temporary worktree (`--head REF`). Manifests are read from the "after" side.
  - A change is a version or source change, an addition or a removal, flagged major or direct, per environment and platform. In Pixi, an environment that catches up to another environment's version has an update, never a removal.
- **Affected packages** are the changed packages plus every package that depends on the version that changed, found by walking the lock graph in reverse.
  - uv extras count only when the lock activates them. Optional npm peers are ignored.
  - In Pixi, C/C++ runtimes stop the walk (`runtime-barrier`). The interpreter is a barrier too: a change to `python` itself is a full run. Its native libraries map to the standard-library modules built on them (`openssl` to `ssl` and `hashlib`).
  - A change to a compiler, or to a tool that builds native extensions (such as `cython` or `pybind11`), is a full run.
  - Environments without the test runner list their changes but never select tests.
- **Usage sites** come from an import scanner per language, built on tree-sitter, which tolerates syntax it does not know instead of falling back to a line scan.
  - Python: absolute and relative imports, `importlib.import_module`, `pytest_plugins` and `pytest.importorskip`. Package names map to import names through the target's installed environments first, read from their files without running them (the nearest `.venv`, and every `.pixi/envs/*` with its `conda-meta`), then through a built-in table, then the normalized name. Below a namespace package, the import name goes down to the first regular package, such as `google.cloud.storage`. `types-*` and `*-stubs` map to the package they type.
  - JS/TS: `import` and `export … from`, `require`, dynamic `import()`, `jest.mock`/`vi.mock` and `/// <reference types>`. Resolution covers relative paths, `.js` to `.ts`, `index` files, tsconfig `paths` with relative `extends`, and npm workspaces. `@types/x` maps to `x`.
  - A package named in source, such as a CLI argument or a URL, is a usage site. One named in a build requirement, a Pixi task or a Babel or PostCSS config forces a full run. One named only in lint or hook configs is tooling only.
- **Test selection** walks the first-party import graph in reverse, from usage sites to test files, and reports each selected test with its chain.
  - Tests are recognised from the runners' own configs, with the deepest config per runner winning: pytest (ini, `pyproject.toml`, tox, `setup.cfg`, Pixi tasks), and vitest/vite, jest, mocha, playwright and cypress, read statically. A computed value falls back to the runner's default, with a warning.
  - An affected `conftest.py` selects every test below it. End-to-end tests are selected whenever affected app code exists.
- **A full run always names its reason**: the interpreter, a compiler or an extension build tool changed, a test runner or one of its plugins changed, a global or setup file is affected, a config names a changed package, `typescript` changed under `ts-jest`, or a lock cannot be parsed.
- **Changes that select nothing are classified**: tooling only, type stubs only, covered, unreferenced, or no visible impact.
- **Untested files** are affected non-test files that no test reaches, even transitively. **Exposed files** are those clients reach directly: Python entry points, the top-level package `__init__`, route decorators and Click/Typer commands; npm `main`/`module`/`exports`/`bin` (mapped from `dist/` back to `src/`), Next.js routes and pages, and Express-style handlers; plus the `exposed` globs, which `internal` globs override.
- **Flow verification** (`--verify`) checks each untested path against the installed "after" environment without running the target's code. Python checks run in the project interpreter and JS checks in node, with the input given as JSON on stdin. Every reference must still resolve, and every direct call must still fit its signature. The outcome is verified, partial, broken or unchecked; a package that isn't installed is unchecked, never broken. `--smoke` imports untested modules and runs `tsc --noEmit` for each owning tsconfig.
- **Running tests** (`depsmith test`) uses `uv run pytest`, `pixi run test` or `pixi run -e ENV pytest`, and `npx vitest|jest|mocha|playwright|cypress` grouped by workspace, with `npm run test` as the fallback. `test-command` and `full-test-command` override them.
- **Exit statuses** follow plan §4: 2 for usage or git errors, 3 for a failing test command (with its status reported), and 4 for a policy failure (untested exposed code under `untested-exposed = "fail"`, or a broken flow under `verify-broken = "fail"`).
- **Reach verdicts.** Each finding gets `reached` (with its usage sites and test count), `dev-tool`, `by-name`, `runtime`, `not-reached`, or `unknown` (the lock is unsupported or the analysis failed).
  - A `not-reached` finding is not actionable: a security update leaves it alone, unless `[impact] unreachable = "fix"` and a fix exists. `--no-impact` skips the analysis. Policy gates and suppressions still apply, and a suppression stays a documented, scoped decision.
  - Reach is per package, not per function, so `not-reached` means nothing in the target uses the package at all.
- **Configuration** lives in `depsmith.toml`. `[impact]` holds the shared keys (`enabled`, `unreachable`, `untested-exposed`, `verify-broken`). `[impact.python]`, `[impact.pixi]` and `[impact.npm]` hold the ecosystem's keys:
  - lists that extend the built-in defaults: always-full, tooling, ignore, global-files, e2e-tests, test-patterns, exclude-tests, internal, exposed, import-map;
  - commands: test-command, full-test-command;
  - scope: platforms, environments, runtime-barrier;
  - behaviour: strict-unreferenced, smoke, verify, no-default-lists.

  In the lists, `"!name"` removes a default, and names match in normalised form. `init` reports each target's test setups and suggested `exclude-tests`, and writes only decisions.
- **Ecosystems.** uv, Pixi and npm come first. Each later adapter gains impact analysis when its language's import scanner exists.

## Considered options

- **Ruff's Python parser and oxc for JS/TS**: rejected. Ruff's parser isn't published on crates.io, which depsmith-core publishes to, and one parsing stack for both languages keeps the scanners alike.
- **Hand-written scanners**: rejected. They need a line-scan fallback for syntax they don't know, and they miss imports that a parser sees.
- **Reporting selected tests without running them**: rejected. `depsmith test` gives CI one command that runs exactly what the analysis selected, with its exit status mapped to plan §4.
- **One flat `[impact]` table**: rejected. The ecosystems' lists differ, and prefixed keys would multiply.
- **Function-level reachability**: rejected. A call graph across ecosystems is costly and imprecise. Package-level reach is conservative on purpose.
- **Suppressing not-reached findings automatically**: rejected. A suppression stays a reviewed decision. The verdict only makes such a finding non-actionable for a security update, and `unreachable = "fix"` restores it.
- **Commits, pull requests or tickets for the result**: rejected. Existing pipeline steps own commits and PRs. `check|update --impact` attaches the result to the proposal instead.
