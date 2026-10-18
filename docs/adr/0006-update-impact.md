---
status: proposed
---

# Proposals report their update impact: the tests a dependency change can break, and the code no test reaches, verified statically

A proposal says which packages move, but not what in the target's own code they reach. A reviewer either runs every test or trusts a green run that never touched the changed package, and a vulnerable package nothing imports looks as urgent as one on a request path. So depsmith reports the **update impact** of a lock change: the code that uses the changed packages, the tests that reach that code, and the affected code that no test reaches, which it checks statically against the candidate.

`check --impact`, `update --impact` and the matching Python option add update impact to a proposal. `depsmith impact` analyses a lock change depsmith did not make (base and head revisions, the staged lock, or two lock files), so a Dependabot or Renovate pull request gets the same report. The design follows hurt-locker 0.3.1, which ships this analysis for uv, Pixi and npm.

## Consequences

- **Changed packages and their dependents come from the lock graph** (ADR 0005), compared per platform and, for Pixi, per environment, then walked in reverse.
  - A package that one environment catches up on (a version another environment already had) is an update there, never a removal or no change.
  - The interpreter is a barrier. A change to `python` or `libpython` is a full run. The interpreter's native libraries map to the standard-library modules built on them: `openssl` maps to `ssl` and `hashlib`, `libsqlite` to `sqlite3`.
  - C/C++ runtimes (`libgcc`, `libstdcxx`, `vc14_runtime`, …) are reported but not propagated, unless `runtime-barrier = false`.
  - Changes only in environments without the test runner are listed and select nothing.
- **Usage sites** are found by an import scanner per language over first-party code, walked in reverse through the first-party import graph to the test files.
  - Python is parsed as an AST, including relative imports, `importlib.import_module`, `pytest_plugins` and `pytest.importorskip`. Distribution names map to import names through the target's environment first, then through a reviewed table (`pyyaml` → `yaml`, `pytorch` → `torch`).
  - JS/TS covers `import`, `require`, dynamic `import()`, `jest.mock`/`vi.mock`, `tsconfig.json` `paths` and npm workspaces. `@types/x` maps to `x`.
  - A package named in source (a subprocess argument, a database URL) or in a non-code config (a task, a build requirement, a Babel plugin) is referenced by name. Source references are usage sites. Config references force a full run that names the file.
- **Tests are recognised from the test runners' own configuration**: pytest, vitest, jest, mocha, playwright and cypress. The deepest config wins for the files under it, so a monorepo with one config per package works. A JS/TS config value that cannot be read statically falls back to the runner's default, with a warning. Nothing about tests is repeated in `depsmith.toml`.
- **A full run is required, with its reason named**, when:
  - a test runner, one of its plugins or its environment changes, directly or transitively (`pluggy` → `pytest`, `esbuild` → `vite` → `vitest`);
  - an affected file configures every test (a setup file, `vite.config.ts`, a `conftest.py` selects everything below it);
  - a compiler or extension build tool changes;
  - a package referenced by name in a non-code config changes;
  - the lock cannot be parsed.
- **Changes that select no tests** are classified: tooling only (linters, formatters, type checkers), type stubs only, covered through an importer, unreferenced (`strict-unreferenced` forces a full run), and no visible impact (not in the tree of anything declared).
- **Reachability verdicts.** Each changed package gets one, and so does each package with a finding when scanning is on:
  - reached: imported directly or through an imported package, with the dependency path and the files;
  - reached only as a tool, by name, or as a C/C++ runtime;
  - not reached.

  Reachability is per package, not per function. A package in an imported framework's tree counts as reached. A not-reached finding is annotated in the scan report and in ADR 0004's security updates. It is never suppressed automatically: a suppression stays a documented, scoped decision.
- **Untested paths.** An affected non-test file that no test imports, even transitively, is listed with its chain, whether or not a full run was chosen.
  - **Exposed code** is marked heuristically from entry points, the top-level package `__init__.py`, web routes, CLI commands, `package.json` `main`/`exports`/`bin`, and `exposed` globs. `internal` globs override it.
  - If end-to-end tests were selected, gaps are listed but not counted as untested exposed code.
- **Static verification** (`--verify`) runs against the candidate's installed environment and executes none of the target's code.
  - Python: every reference on the path must still resolve, from the file where the path leaves first-party code, then hop by hop through each intermediate package's installed source. Every direct call must still fit the signature: keywords accepted, positional count, newly required arguments. Compatibility shims in `try/except ImportError` are skipped.
  - npm: every named or default import must still be exported, and the package must load with its updated dependencies.
  - The outcome is verified, partial (a native or npm transitive hop), broken or unchecked. A package that isn't installed is unchecked, never broken. Verification proves the API shape, not behaviour.
  - It uses the disposable environment that optional install validation (plan §3) already creates. Without one, it reports unchecked.
- **Policies.** Configured in `depsmith.toml` under `[impact]`, overridable per ecosystem and per target:
  - `untested-exposed = "warn" | "fail"`;
  - `verify-broken = "fail" | "warn"`.

  A failing policy rejects the proposal with exit status 4. A verified path passes with a notice; a partial one still counts as untested.
- **Lists extend defaults.** `always-full`, `tooling`, `ignore`, `global-files`, `e2e-tests`, `test-patterns`, `exposed`, `internal`, `exclude-tests` and `import-map` extend built-in lists. `"!name"` removes one default, and `no-default-lists` drops them all. Package names match in normalised form.
- **Selected tests are reported, not run.** The report gives each selected test with the chain that justifies it, and the command to run them (argv and working directory) per target. Running them belongs to the configured post-install project checks deferred to 0.2.
- **Ecosystems.** uv, Pixi (lock format 4 and later) and npm come first, matching hurt-locker. Cargo and conda-lock follow through the same seam.
- **Seam.** Impact analysis is a separate extension interface (plan §2), with one *impact analyzer* per language and ecosystem. It consumes the lock graph and a target's sources, and returns usage sites, test selection, untested paths and verification results as owned, serialisable data. The orchestrator has no manager-specific branches. The JSON report gains an `impact` section, and its schema version is raised.
- **Cross-repository impact** (a shared package consumed by other repositories) is out of scope. Each consumer is analysed when its own lock picks up the change.

## Considered options

- **Run hurt-locker as an external tool**: rejected. It adds a Python runtime and a second configuration file to every target, and Python API callers would not get structured, parity-checked results.
- **Function-level reachability**: rejected. Call-graph analysis across ecosystems is costly and imprecise. Package-level reachability is conservative on purpose, so not reached means nothing in the target uses the package at all.
- **Suppress findings on unreached packages automatically**: rejected. A suppression must stay a reviewed, scoped decision. The verdict is evidence for it, not a substitute.
- **Run the selected tests as part of validation now**: deferred. Project checks after installation are a 0.2 feature, and this report gives them their input.
- **Bring in hurt-locker's org-wide Dependabot alert fixer**: rejected. Opening branches, tickets and pull requests is PR management, which stays outside depsmith's scope; existing pipeline steps own commits and PRs.
