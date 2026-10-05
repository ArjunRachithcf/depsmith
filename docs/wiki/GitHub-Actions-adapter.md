# GitHub Actions adapter

Workflow files under `.github/workflows/` are targets, such as
`github-actions:.github/workflows/ci.yml`. The adapter updates remote `uses:`
references to actions and reusable workflows. Local actions, Docker references,
expressions, script contents, runner labels and action inputs are left alone.

- **Release lines:** a reference moves to the newest release in its current
  major line. A newer major line is reported as a suggestion naming its newest
  release. `--accept owner/name` moves every reference to that repository to
  it, each in its own style: `v4` becomes `v5`, `v4.2` becomes `v5.1`,
  `v4.2.1` becomes `v5.1.0`, and a commit pin becomes the new release's commit
  with its `# vX.Y.Z` comment. When the matching alias tag does not exist,
  name a release with `--accept owner/name=TAG` instead. `--upgrade --package
  owner/name` also still moves to the newest major. One `--accept` makes one
  kind of change: while a reference can move to a newer major, references
  already on it are left alone, and pinning is a second `--accept` afterwards.
  The suggestion lists what each reference becomes, and commit pin hints wait
  until no move is pending.
- **Style:** exact tags stay exact tags, major tags such as `v4` stay major
  tags, and full commit SHAs stay SHAs. A trailing `# vX.Y.Z` comment equal to
  the old tag is updated with the reference; an updated SHA gets a
  `# vX.Y.Z` comment naming its new release, replacing any existing comment.
- **Commit pins:** each tag reference gets a suggestion to pin it to its
  release commit, citing the commit and the most specific release tag on it.
  When no newer major is published, `--accept owner/name` rewrites every tag
  reference to that repository as `owner/name@<sha> # vX.Y.Z`, and later
  updates keep the pin style (with a newer major, it moves to the major
  first; accept again to pin). `--accept owner/name=TAG` must name a
  published release or a release line: a published tag is written as given
  (commit pins get its commit), and a line such as `v5` or `v5.0` without its
  own tag resolves to its newest release in each reference's style. `--accept owner/name=SHA` writes that commit as given. Branches
  cannot be pinned this way, and a commit pin with no newer major has
  nothing to accept.
- **Unresolved references:** branches such as `@main`, SHAs that match no
  release, and missing alias tags stay unchanged and are listed under
  `unresolved` with the reason.
- **Authentication:** `GITHUB_TOKEN` or `GH_TOKEN`, when set, authenticates
  GitHub API requests; anonymous requests are rate-limited.

Only the reference text changes; the rest of the YAML is kept byte for byte,
and a layout that cannot be edited that way is rejected rather than rewritten.
