# Cargo adapter

## Targets

Each manifest that owns a `Cargo.lock` is a target: a workspace root
(`[workspace]`) or a package that no enclosing workspace lists as a member,
such as `cargo:Cargo.toml` or `cargo:tools/xtask/Cargo.toml`. Workspace
members are not targets of their own; their manifests are part of the
workspace's target. Member globs in `workspace.members` and
`workspace.exclude` are honoured. The `target/` directory is never walked.

## Updates

An **update** runs `cargo update` inside the stage and keeps every declared
requirement. It needs cargo 1.84 or newer; older versions are rejected with
exit 2.

- **MSRV-aware:** resolution falls back to versions compatible with the
  declared `rust-version`
  (`CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback`). A crate held back
  because its newer release needs a newer Rust is reported as a suggestion
  that names the version and the Rust it requires.
- `--package NAME` (repeatable) updates only the named direct dependencies
  (`cargo update -p`). Names match case-insensitively with `-` and `_`
  equivalent, and a renamed dependency matches by its key or by its crate.
- **Git dependencies** stay at their locked commit unless `--refresh-git` is
  given.
- The candidate lock is checked with `cargo update --workspace --locked`, and
  manifests must not change.
- `--upgrade` is not supported: change a requirement with `--accept`.

## Suggestions

Declarations are the `[dependencies]`, `[dev-dependencies]` and
`[build-dependencies]` tables of the root and member manifests, including
platform-specific `[target.'cfg(...)'.dependencies]` and
`[workspace.dependencies]`. Path, Git and `workspace = true` dependencies have
no requirement to suggest for, and dependencies from other registries are not
consulted.

A bare requirement such as `serde = "1.0"` is a caret requirement: it
excludes the next incompatible release, so a newer major (or, before 1.0, a
newer minor) release is suggested with **evidence** from the crates.io sparse
index: the release and its checksum. Yanked releases and pre-releases are
never cited, and `>=` bounds alone never produce suggestions.

## Accepting a suggestion

`--accept NAME` rewrites the requirement in its own style at its declared
precision: `1.0` → `2.5`, `1` → `2`, `^0.2.3` → `^2.5.1`, `=1.2.3` → `=2.5.1`,
`~1.2` → `~2.5` and `1.*` → `2.*`. Comments and layout are kept. Compound
requirements such as `>=1, <2` need `--accept NAME=REQUIREMENT`, which must be
a valid Cargo requirement.

## Scanning

Crates from crates.io are scanned as `pkg:cargo/NAME`. Git and path sources
stay unassessed, and workspace members are the project itself, not
dependencies.

## Dependency introducers

A changed crate that the target does not declare is reported with the direct
dependencies that pull it in. The graph comes from `Cargo.lock`, which records
dependency names only. A crate locked at several versions is spelled
`name version`.
