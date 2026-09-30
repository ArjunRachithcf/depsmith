# GitHub Actions adapter

Workflow files under `.github/workflows/` are targets, such as
`github-actions:.github/workflows/ci.yml`. The adapter updates remote `uses:`
references to actions and reusable workflows. Local actions, Docker references,
expressions, script contents, runner labels and action inputs are left alone.

- **Release lines:** a reference moves to the newest release in its current
  major line. Newer major lines are reported as separate suggestions; select
  the repository with `--upgrade --package owner/name` to move to one.
- **Style:** exact tags stay exact tags, major tags such as `v4` stay major
  tags, and full commit SHAs stay SHAs. A trailing `# vX.Y.Z` comment equal to
  the old tag is updated with the reference; an updated SHA gets a
  `# vX.Y.Z` comment naming its new release, replacing any existing comment.
- **Commit pins:** each tag reference gets a suggestion to pin it to its
  release commit, citing the commit and the most specific release tag on it.
  `--accept owner/name` rewrites every tag reference to that repository as
  `owner/name@<sha> # vX.Y.Z`, and later updates keep the pin style.
  `--accept owner/name=REF` writes a tag or SHA of your choice instead.
  Branches cannot be pinned this way, and a reference that already is a
  commit pin has nothing to accept.
- **Unresolved references:** branches such as `@main`, SHAs that match no
  release, and missing alias tags stay unchanged and are listed under
  `unresolved` with the reason.
- **Authentication:** `GITHUB_TOKEN` or `GH_TOKEN`, when set, authenticates
  GitHub API requests; anonymous requests are rate-limited.

Only the reference text changes; the rest of the YAML is kept byte for byte,
and a layout that cannot be edited that way is rejected rather than rewritten.
