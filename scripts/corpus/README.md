# Corpus

depsmith run end to end over real repositories: about 30 per package manager,
each pinned to a commit in `repos.toml`. A pass runs each step in a fresh copy
of the repository:

1. `discover`
2. `check`
3. `update --apply`, then:
   - the native tool's lock check;
   - a re-check that must find nothing pending;
   - the impact walk.
4. `fix --apply`, with a scan, then the native lock check.

Each step adds one row to `results.tsv`.

```sh
cargo build --release -p depsmith
export CORPUS_WORK=/path/with/space CORPUS_TOOLS=/path/to/depsmith-tools
python3 -I scripts/corpus/run.py run --pass 1            # every repository
python3 -I scripts/corpus/run.py run --pass 1b --only uv owner/repo
python3 -I scripts/corpus/report.py --pass 1 --against 0
```

`CORPUS_TOOLS` is a tool cache that `depsmith init --fetch-tools` filled
(`DEPSMITH_TOOLS_DIR`). Each repository gets its own package caches and
temporary directory, which are deleted after it, so disk use stays bounded.

`report.py` counts each step's outcome per manager and groups failures by a
normalised first line. `triage.toml` classifies the failures that are not
depsmith's, each with its reason:

- `env`: a blocked host or the disk.
- `repo`: the repository cannot be resolved as it stands.
- `design`: depsmith declines by design.

The loop:

1. Fix the largest remaining `bug` or `triage` cluster, test first.
2. Re-run the cluster with `--only`.
3. Run a full pass.

It ends when a full pass has no `bug` or `triage` rows.

## Hosts that block what depsmith needs

- `CORPUS_ACTIONS_VIA_GIT=1`: where the GitHub REST API is out of reach, the
  Actions adapter runs through `crates/core/tests/corpus_actions.rs`. That
  driver reads releases from `git ls-remote --tags`.
- `CORPUS_FAKE_GRYPE=1`: where Grype's database host is out of reach,
  `fake_grype.py` stands in, backed by a checkout of
  github.com/github/advisory-database. Build its index with
  `fake_grype.py index CHECKOUT INDEX.json`, then wrap it as
  `$CORPUS_TOOLS/fake-grype/grype`.

`run.py pin CANDIDATES.jsonl` turns candidates into `repos.toml` entries. Each
line of the file is `{"manager", "repo", "manifest"}`. It keeps up to 30 per
manager whose target is discovered and whose lock passes the native check at
the pinned commit.
