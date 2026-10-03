# npm adapter

## Targets

Each `package.json` that owns an npm lock is a target, such as
`npm:package.json`: a package with a `package-lock.json` (lockfileVersion 2 or
3, npm 7 or newer) or an `npm-shrinkwrap.json` beside it, that no enclosing
workspace lists as a member. Members of `workspaces` (its globs, in either the
array or the `{ "packages": [...] }` form) are part of the root's target.
A package with only a `yarn.lock` or `pnpm-lock.yaml`, or with no lock, is not
claimed. `package-lock.json`, `npm-shrinkwrap.json` and `.npmrc` are staged
even when ignored by Git, and `node_modules/` is never walked.

## Updates

An **update** runs `npm update --package-lock-only` inside the stage: it
re-resolves within the declared ranges and changes only the lock. Package
scripts never run (`--ignore-scripts`), and no audit or funding requests are
made.

- `--package NAME` (repeatable) updates only the named direct dependencies.
- **Git dependencies** stay at their locked commit unless `--refresh-git` is
  given: every other locked package is updated by name instead.
- `--cooldown-days N` becomes npm's `--before`: only releases at least N days
  old are chosen, so an already-locked newer release can move back.
- The candidate is checked with `npm install --package-lock-only` (it must not
  change the lock), and the manifests must not change.
- `--upgrade` and `--install` are not supported: change a range with
  `--accept`.

Your npm configuration (user and project `.npmrc`: registry, scopes, auth)
applies as it would for a plain `npm update`.

## Suggestions

Declarations are the `dependencies`, `devDependencies` and
`optionalDependencies` of the root and member `package.json` files. Git,
GitHub shorthand, `file:`, `link:`, `workspace:`, URL and `npm:` alias specs
are not looked up.

A pin or range that excludes a newer release (`2.0.0`, `^1.2.3` against a new
major, `~1.2`) is suggested with **evidence** from the project's registry
(`registry=` in its `.npmrc`, else registry.npmjs.org): the newest stable
release's tarball and integrity hash. Pre-releases and deprecated releases are
never cited, and open ranges such as `>=1` or `*` never produce suggestions.

## Accepting a suggestion

`--accept NAME` rewrites the range in its own style at its declared precision:
`2.0.0` → `2.1.3`, `^1.2.3` → `^2.5.1`, `~1.2` → `~2.5`, `1.x` → `2.x`. Only that
value changes in `package.json`; formatting and key order are kept. Compound
ranges such as `>=1 <2` need `--accept NAME=RANGE`, which must be a valid npm
range.

## Tools

`depsmith init` can install npm with a pinned Node.js LTS (its bundled npm is
the tested version), checksum-verified, into the tool cache; npm then runs with
that Node first on `PATH`.

## Scanning

Packages from registry.npmjs.org are scanned as `pkg:npm/NAME` (scoped names
as `pkg:npm/%40scope/name`). Git and other sources stay unassessed, and
workspace members are the project itself.
