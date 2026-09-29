# CI integration

In CI, depsmith runs without prompts, resolves and applies in one job, and
reports through exit statuses and JSON. Committing the result and opening a
pull request belong to your pipeline.

```sh
depsmith check  --root "$PROJECT_DIR" --all --non-interactive --json > "$REPORT_DIR/check.json"
depsmith update --root "$PROJECT_DIR" --all --apply --yes --non-interactive --json > "$REPORT_DIR/update.json"
```

- Select targets explicitly (`--target`, `--all`, or a saved selection);
  ambiguity is an error.
- Keep reports **outside** the project while preparing and applying, so writing
  a report does not change the inputs the proposal was prepared from.
- `--json` keeps stdout valid JSON and sends diagnostics to stderr;
  `--markdown` gives a readable summary with diffs, for example for a job
  summary.
- A proposal cannot be carried between jobs: prepare and apply in the same job.
- Scanning is opt-in: install Grype and add `--scan` and the policy flags.

## Exit statuses

| Exit | Meaning |
|---|---|
| 0 | Success; `check` found nothing pending |
| 1 | `check` found pending changes |
| 2 | Invalid arguments, configuration or selection |
| 3 | Package manager, validation, scanner or application failure |
| 4 | Policy rejection (vulnerability gate) |
| 5 | Partial success |
| 130 | Interrupted |

## Examples

- [GitHub Actions check](https://github.com/ArjunRachithcf/depsmith/blob/main/docs/examples/github-actions.yml):
  treats exit 1 as available updates and fails on other errors. Full Git
  history is fetched so SCM-derived versions work.
- [Provider-neutral shell job](https://github.com/ArjunRachithcf/depsmith/blob/main/docs/examples/update.sh):
  `sh update.sh /path/to/project /outside/reports check|apply`.
