---
status: proposed
---

# Jenkins plugin pins move within the target's Jenkins core, chosen from update-center data and validated by jenkins-plugin-cli

A `plugins.txt` or `plugins.yaml` line is both the constraint and the resolution (there is no lock; dependencies are resolved when the image is built), so the adapter treats pins like GitHub Actions references: an update moves each pinned plugin to the newest release whose `requiredCore` the target's Jenkins core satisfies, and a release needing a newer core is a hold-back reported as a suggestion, never resolved past. depsmith chooses those versions itself from the update center's `plugin-versions.json` (every release, with `requiredCore` and `releaseTimestamp`), because `jenkins-plugin-cli --available-updates` bounds by core only when a version-specific update center exists and has no release-age cutoff; dependency resolution stays with the native tool, which validates the candidate with `jenkins-plugin-cli --no-download --list --latest false --jenkins-version CORE` and yields its effective plugin set.

## Consequences

- The **Jenkins core** must be known: from `depsmith.toml` (per target path), else from the `FROM jenkins/jenkins:<version>` of a Dockerfile beside the file; without either the target fails, since guessing it would make every hold-back wrong. depsmith never raises the core.
- Ecosystem `jenkins-plugin` orders versions as Jenkins does (`VersionNumber`, covering forms such as `1480.v2246fd131e83`). Plugins have no semantic release lines, so the core is the only bound on an update.
- `--cooldown-days` is supported, applied to `releaseTimestamp` in the same choice.
- Unpinned lines (`id`, `id:latest`) float at image build and are left alone; `--accept id` pins one. `experimental`, `incrementals;…` and URL sources are not looked up.
- Evidence cites the release's download URL and its SHA-256 (base64 in the update center, cited as hex). Mirrors are read the way `jenkins-plugin-cli` reads them (`JENKINS_UC`, `JENKINS_PLUGIN_INFO`).
- The inventory is the effective plugin set. Scan identities use each plugin's `gav` from `update-center.json` (`pkg:maven/<groupId>/<artifactId>`), not a fixed `org.jenkins-ci.plugins` group as first planned, because plugins publish under several group IDs; advisories come from the scanner as for every ecosystem, and plugins without a `gav` stay unassessed.
- `jenkins-plugin-cli` needs Java 17 or newer; init installs the pinned jar and reports a missing Java instead of installing one.

## Considered options

- **Let `jenkins-plugin-cli --available-updates` choose the candidate** (the first plan): rejected; without a version-specific update center it can choose releases the core cannot run, and it cannot apply a cooldown.
- **Resolve dependencies inside depsmith too, with no Java**: rejected; it would reimplement what the image build does with `jenkins-plugin-cli`, and the two could disagree.
- **Raise the core from `jenkins/jenkins` image tags**: rejected; the core is the controller's own upgrade, with its own review.
