---
status: proposed
---

# A target whose resolution would downgrade a package proposes no change

depsmith resolves each target with its native package manager and proposes the result. Sometimes the resolver moves a package to an older version while updating, for example when a newer release of one package needs an older one of another. A corpus run over hundreds of public repositories found this in several places: a Cargo.lock whose only `cpp_demangle` went from 0.5.1 to 0.4.5, a `psm` going from 0.1.25 to 0.1.24, an npm lock moving `asn1.js` from 5.4.1 to 4.10.1, and a uv.lock moving `aiofile` back. A downgrade is not an update. It can reintroduce fixed bugs or vulnerabilities, and its reason is hidden in the resolver. So depsmith holds such a target back: it proposes no change for it and says why.

## Consequences

- **What counts:** a dependency change whose new version orders before its old one, by the ecosystem's version scheme. Copies on different release lines are paired first, so a copy that goes away is not read as a downgrade. npm inventory entries carry no install path yet, so when one nested copy goes away and another appears under a different package, the two can still pair; such a target is held back too, which errs on the side of no change.
- **Held back:** the target gets no file changes and no dependency changes; its candidate is its current lock. Each downgrade is listed as an unresolved reference with the reason "no upgrade fix for now", naming the package and both versions, and the proposal's validation notes say the target was held back. Suggestions about the target's declared constraints are still reported, because they do not depend on the resolution.
- **Allowing a downgrade is the user's explicit choice:** `--allow-downgrades` (option `allow_downgrades`) proposes such a resolution as usual and names each downgrade in the validation notes. Without it, the held-back note says that this option would propose it.
- **Security updates follow the same rule.** A security update that would downgrade something is held back too. A held-back target is scanned as its lock stays, so `--fail-on` still applies and a security update reports its findings as not fixed.
- **Only versions the scheme reads count:** a Git commit, a path or an unknown version is never a downgrade.
- **Check never looks up to date:** a held-back target is listed in the proposal's `held_back`, the report says so on its own line, and `depsmith check` exits 6, even when other targets have pending changes, so a lock that one package keeps from updating cannot go stale unnoticed.
- **Rejected alternatives:**
  - A validation note on an otherwise normal proposal would still let a downgrade be applied without anyone noticing.
  - Holding back only the downgraded package would need a second, constrained resolution per target, and the resolver might then fail or move something else. That may come later.
