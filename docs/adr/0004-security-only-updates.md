---
status: proposed
---

# Security updates move only vulnerable packages, through the native tool, and change constraints only by acceptance

Some teams accept dependency changes only when they fix a vulnerability, so a **security update** (`update --security-only`, aliased as `depsmith fix`, and `check --security-only`) scans the baseline, then asks the native tool to move each package with an actionable finding to its **fix version**, and changes nothing else on purpose. Where the tool cannot reach that version without a constraint change, the outcome is a suggestion citing the advisory, the fix version and the **dependency path**, and constraints change only through acceptance, as they do today.

## Consequences

- **Actionable findings** have at least one fixed release, are not suppressed, and meet `--min-severity` (a new option, separate from the `--fail-on` policy gate). `--security-only` implies `--scan`.
- **Fix version** is chosen from the advisory's fixed releases by `security_target`: `lowest` (the default), `patch` (newest release in the lowest fix's line) or `newest` (newest the constraints allow). Findings gain a field for the fixed releases Grype reports (`vulnerability.fix.versions` and its state); today only `matchDetails` is kept.
- **Precise update** becomes an adapter capability.
  - Cargo (`cargo update -p NAME --precise VERSION`) and uv (`uv lock --upgrade-package NAME==VERSION`) have it.
  - npm, Pixi and conda-lock do not. There the package moves to the newest release its constraints allow, and the proposal says so.
  - If that is still below the fix version, the finding is a blocked fix (a suggestion, not a change).
  - No adapter works around a missing capability by editing declarations.
- **Targets can be transitive**, so security updates select packages from the inventory, not only from declarations.
  - Each adapter must show, in its live tests, that its tool moves a nested package named this way.
  - Until it does, its transitive fixes are reported as blocked.
- **Other packages may still move.** Re-solving can move other packages (a target's own dependencies). Every package that moved without being targeted is listed in the proposal, and the baseline/candidate scan must show each targeted finding resolved and none introduced.
- **Blocked fixes name the declaration to change.**
  - When the blocker is a declared constraint, the suggestion names it, and it is accepted with `--accept NAME=REQUIREMENT` as today.
  - npm `overrides` and parent requirements need a new kind of acceptance. Until one exists, those suggestions describe the change without applying it.
- **Dependency paths come from each target's lock graph** (ADR 0005), read from `Cargo.lock`, `uv.lock`, `package-lock.json`, `pixi.lock` or conda-lock.
  - The native tree command (`cargo tree`, `uv tree`, `npm ls`, `pixi tree`) is used only where a lock records none.
  - Without either, the path is reported as unknown, and a vulnerable direct dependency is still updated.
- **Unfixable findings** are reported with their advisory and change nothing; policy gates and expiring suppressions still apply.
- **Unassessed packages** are counted and reported, never as clean, and fail the run only if a policy says so.

## Considered options

- **Change blocking constraints automatically in security-only mode**: rejected. A security update would widen what a project allows without review.
- **Force exact versions on npm or Pixi by rewriting declarations** (`npm install NAME@VERSION`): rejected. It is a constraint change, and per-tool special casing that must track each tool's evolution.
- **Reject a candidate when untargeted packages move**: rejected. Re-solving tools move dependencies legitimately. The proposal lists them, and the scan verifies them.
- **Default to the policy threshold, or to High and Critical only**: rejected. Teams choose with `--min-severity`.
- **Fail the run when packages are unassessed**: rejected as a default. Pixi and conda repositories would fail routinely.
