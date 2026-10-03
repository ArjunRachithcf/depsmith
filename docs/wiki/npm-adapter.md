# npm adapter

## Targets

Each `package.json` that owns an npm lock is a target, such as
`npm:package.json`: a package with a `package-lock.json` (lockfileVersion 2 or
3, npm 7 or newer) or an `npm-shrinkwrap.json` beside it, that no enclosing
workspace lists as a member. Members of `workspaces` (its globs, in either the
array or the `{ "packages": [...] }` form; `**` matches any depth outside
`node_modules` and hidden directories, and `!glob` removes members matched by
earlier globs) are part of the root's target. A root with `workspaces` but no
lock is not claimed, and neither are its members.
A package with only a `yarn.lock` or `pnpm-lock.yaml`, or with no lock, is not
claimed. `package-lock.json`, `npm-shrinkwrap.json` and `.npmrc` are staged
even when ignored by Git, and `node_modules/` is never walked. A UTF-8 byte
order mark at the start of a `package.json` or lock is accepted, as npm does.

## Updates

An **update** runs `npm update --package-lock-only` inside the stage: it
re-resolves within the declared ranges and changes only the lock. Package
scripts never run (`--ignore-scripts`), and no audit or funding requests are
made.

- `--package NAME` (repeatable) updates only the named direct dependencies.
- **Git dependencies** stay at their locked commit unless `--refresh-git` is
  given: every other locked package is updated by name instead (its
  `node_modules` folder name, so aliases are covered). If naming them all
  would exceed the command line (8191 characters for `npm.cmd` on Windows),
  only the declared dependencies are named, and the proposal says so; if even
  those are too many, select packages with `--package` or use `--refresh-git`.
- `--cooldown-days N` becomes npm's `--before`: only releases at least N days
  old are chosen, so an already-locked newer release can move back.
  Suggestions apply the same cooldown, so they never cite a release npm would
  refuse.
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
major, `~1.2`) is suggested with **evidence** from the registry npm would use:
the newest stable release's tarball and integrity hash (as npm records it,
such as `sha512-…`; `sha1:…` for releases older than npm 5). Pre-releases and
deprecated releases are never cited, and open ranges such as `>=1` or `*`
never produce suggestions. With `--cooldown-days`, releases younger than the
cooldown are not cited either; their publish times come from the registry's
full metadata.

The registry is read from npm's configuration, most specific first:
`npm_config_registry`, the project's `.npmrc`, then the user's
(`NPM_CONFIG_USERCONFIG`, else `~/.npmrc`), else registry.npmjs.org.
`@scope:registry=` sends scoped packages to their own registry. Values may be
quoted. A repository chooses registries but never what secrets reach them:
`${VAR}` (`${VAR?}` when it may be unset) is expanded only in the user's
`.npmrc`, and lookup credentials (`//host/path/:_authToken` or `:_auth`) are
read only from it, at lookup time, never from the project's `.npmrc` and never
into the proposal. A registry that is not a URL, or still names `${VAR}`,
fails the target instead of falling back to the public registry.
`username`/`_password` credentials are not supported for lookups.

## Secrets and untrusted changes

Every update and check runs the real `npm` in the stage, and npm reads the
project's `.npmrc` as usual: it expands `${VAR}` from your environment and
sends credentials to the registries that file names. A change to `.npmrc`,
such as one in a pull request, can therefore direct your tokens to another
server, exactly as `npm install` would. depsmith does not try to prevent this.
Run it on untrusted changes only without secrets in the environment, for
example in a CI job that does not receive them (GitHub withholds secrets from
pull requests opened from forks).

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
