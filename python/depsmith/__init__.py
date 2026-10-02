"""Typed Python interface to the same Rust engine used by the depsmith CLI.

Discover targets, prepare a reviewable proposal, inspect its file and
dependency changes, suggestions and scans, then apply it exactly as reviewed.
Functions never prompt or exit the interpreter; failures raise
:class:`ConfigurationError`, :class:`OperationError`,
:class:`StaleProposalError` or :class:`PolicyError`. Terms follow the
project glossary (``CONTEXT.md``).
"""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from dataclasses import asdict, dataclass
from os import PathLike
from typing import Any

from . import _native

# Re-exported exception types (redundant aliases mark them as public).
from ._native import ConfigurationError as ConfigurationError
from ._native import OperationError as OperationError
from ._native import PolicyError as PolicyError
from ._native import StaleProposalError as StaleProposalError

Path = str | PathLike[str]
"""A filesystem path accepted by the API."""


@dataclass(frozen=True)
class Target:
    """A manifest or workflow file that depsmith updates.

    Attributes:
        id: Stable identifier ``manager:path``, such as ``pixi:pixi.toml``.
        manager: The package manager that owns the target.
        manifest: Path of the file, relative to the repository root.
    """

    id: str
    manager: str
    manifest: str


@dataclass(frozen=True)
class Suppression:
    """A documented, scoped exception for one advisory.

    Suppressed findings stay in reports and are only excluded from policy
    gates. A reason and a package or target scope are required.

    Attributes:
        id: Advisory identifier, such as ``GHSA-…`` or ``CVE-…``.
        reason: Why the finding is acceptable.
        package: ``ecosystem:name`` scope, such as ``pypi:urllib3``.
        target: Target identifier scope, such as ``pixi:pixi.toml``.
        expires: Last day (``YYYY-MM-DD``, UTC) the suppression applies.
    """

    id: str
    reason: str
    package: str | None = None
    """``ecosystem:name``, e.g. ``pypi:urllib3``."""
    target: str | None = None
    expires: str | None = None
    """Last day (``YYYY-MM-DD``, UTC) the suppression applies."""


@dataclass(frozen=True)
class IdentityMapping:
    """A reviewed statement that a package is the same software as an upstream identity.

    Attributes:
        ecosystem: Ecosystem of the mapped package, such as ``conda``.
        name: Package name in that ecosystem.
        purl: Unversioned Package URL, such as ``pkg:pypi/urllib3``.
        evidence: HTTPS link to the provenance that establishes the mapping.
    """

    ecosystem: str
    name: str
    purl: str
    evidence: str


@dataclass(frozen=True)
class UpdateOptions:
    """Options for preparing, scanning and applying updates.

    ``None`` inherits the repository settings from ``depsmith.toml``;
    explicit values override them.

    Attributes:
        packages: Direct dependencies to update; empty means the whole target.
        accept: ``NAME`` or ``NAME=REQUIREMENT`` suggestions to accept.
        upgrade: Allow the selected packages' constraints to change.
        refresh_git: Allow Git pins to move to newer commits.
        cooldown_days: Minimum release age; rejected where not enforceable.
        install: Also install the candidate's default environment on this host.
        scan: Scan the baseline and candidate for known vulnerabilities.
        fail_on: Lowest severity that rejects the proposal.
        only_new: Apply ``fail_on`` only to introduced findings.
        tools: Executable paths by tool name, such as ``{"pixi": "/opt/pixi"}``;
            ``doctor()`` lists the tool names.
        pixi: Deprecated alias for ``tools["pixi"]``.
        grype: Deprecated alias for ``tools["grype"]``.
        timeout_seconds: Time limit for each backend process.
        identity_mappings: Reviewed identity mappings used when scanning.
        suppressions: Scoped, documented suppressions.
    """

    packages: Sequence[str] | None = None
    accept: Sequence[str] | None = None
    """``NAME`` restyles the pin to the evidenced newest release; ``NAME=REQUIREMENT`` replaces it."""
    upgrade: bool | None = None
    refresh_git: bool | None = None
    cooldown_days: int | None = None
    install: bool | None = None
    scan: bool | None = None
    fail_on: str | None = None
    only_new: bool | None = None
    tools: Mapping[str, str] | None = None
    pixi: str | None = None
    grype: str | None = None
    timeout_seconds: int | None = None
    identity_mappings: Sequence[IdentityMapping] | None = None
    suppressions: Sequence[Suppression] | None = None

    def _json(self) -> str:
        return json.dumps(
            {key: value for key, value in asdict(self).items() if value is not None}
        )


@dataclass(frozen=True)
class Package:
    """One resolved package in an inventory.

    Attributes:
        ecosystem: Ecosystem of the identity, such as ``conda`` or ``pypi``.
        name: Package name as the ecosystem spells it.
        version: Resolved version; empty when the lock records none.
        artifact: The exact artifact or reference resolved.
        platform: Platform resolved for, such as ``linux-64``.
    """

    ecosystem: str
    name: str
    version: str
    artifact: str
    platform: str


@dataclass(frozen=True)
class DependencyChange:
    """A package that differs between the baseline and the candidate.

    Attributes:
        before: The package before the update; ``None`` when added.
        after: The package after the update; ``None`` when removed.
    """

    before: Package | None
    after: Package | None


@dataclass(frozen=True)
class FileChange:
    """The exact new content of one file in a proposal.

    Attributes:
        target: Identifier of the target the file belongs to.
        path: Path relative to the repository root.
        before: Current content; ``None`` when the file does not exist yet.
        after: Content that applying the proposal writes.
        diff: Unified diff from ``before`` to ``after``.
    """

    target: str
    path: str
    before: str | None
    after: str
    diff: str


@dataclass(frozen=True)
class Suggestion:
    """An evidence-backed report that a declared constraint is worth revisiting.

    Attributes:
        target: Identifier of the target that declares the constraint.
        package: The constrained direct dependency.
        requirement: The declared requirement, as written.
        reason: Why it is reported and how to act on it.
        evidence: Sources of the claim, such as artifact URLs with SHA-256.
    """

    target: str
    package: str
    requirement: str
    reason: str
    evidence: tuple[str, ...] = ()


@dataclass(frozen=True)
class Unresolved:
    """A reference left unchanged because it could not be resolved.

    Attributes:
        target: Identifier of the target containing the reference.
        package: The referenced package, such as an action repository.
        reference: The reference as written, such as ``org/repo@main``.
        reason: Why no release could be matched.
    """

    target: str
    package: str
    reference: str
    reason: str


@dataclass(frozen=True)
class Failure:
    """A target whose candidate could not be prepared.

    Attributes:
        target: Identifier of the failed target.
        message: What went wrong, with redacted backend output.
        code: CLI exit status category of the failure.
    """

    target: str
    message: str
    code: int


@dataclass(frozen=True)
class Finding:
    """One advisory matched to one inventory package.

    Attributes:
        id: Advisory identifier.
        namespace: Advisory namespace reported by the scanner.
        package: Name of the affected package.
        version: Version of the affected package.
        severity: Severity as reported, such as ``High``.
        evidence: The scanner's match details, verbatim.
        identity: ``ecosystem:name:platform`` of the package.
        applicability: ``upstream``, or why applicability to this build is unknown.
        artifact: The resolved artifact.
        suppression: The suppression covering this finding, if any.
    """

    id: str
    namespace: str
    package: str
    version: str
    severity: str
    evidence: Any
    identity: str
    applicability: str
    artifact: str
    suppression: Suppression | None

    @classmethod
    def _from_dict(cls, data: dict[str, Any]) -> Finding:
        suppression = data.get("suppression")
        return cls(
            **{
                **data,
                "suppression": Suppression(**suppression) if suppression else None,
            }
        )


@dataclass(frozen=True)
class ScanReport:
    """Vulnerability findings for one target, classified against the baseline.

    Packages that could not be assessed are listed as unknown, never
    treated as clean.

    Attributes:
        target: Identifier of the scanned target.
        baseline_available: Whether a baseline lock existed.
        comparison: ``baseline``, or ``candidate-only`` without a baseline.
        database: Status of the vulnerability database snapshot used.
        findings: Every candidate finding.
        introduced: Findings in the candidate but not the baseline.
        resolved: Findings in the baseline but not the candidate.
        remaining: Findings in both.
        unknown_before: Baseline packages that could not be assessed.
        unknown_after: Candidate packages that could not be assessed.
        applicability: What the findings establish about applicability.
        expired_suppressions: Suppressions that had expired and were ignored.
        policy_passed: Whether the configured policy accepted the findings.
    """

    target: str
    baseline_available: bool
    comparison: str
    database: dict[str, Any]
    findings: tuple[Finding, ...]
    introduced: tuple[Finding, ...]
    resolved: tuple[Finding, ...]
    remaining: tuple[Finding, ...]
    unknown_before: tuple[Package, ...]
    unknown_after: tuple[Package, ...]
    applicability: str
    expired_suppressions: tuple[Suppression, ...]
    policy_passed: bool

    @classmethod
    def _from_dict(cls, data: dict[str, Any]) -> ScanReport:
        data = dict(data)
        for key in ("findings", "introduced", "resolved", "remaining"):
            data[key] = tuple(Finding._from_dict(row) for row in data[key])
        data["expired_suppressions"] = tuple(
            Suppression(**row) for row in data["expired_suppressions"]
        )
        for key in ("unknown_before", "unknown_after"):
            data[key] = tuple(Package(**row) for row in data[key])
        return cls(**data)


@dataclass(frozen=True)
class ApplyResult:
    """What applying a proposal wrote.

    Attributes:
        schema_version: Version of the report format.
        applied: Files written, relative to the repository root.
        partial: Whether only the successful targets were applied.
    """

    schema_version: int
    applied: tuple[str, ...]
    partial: bool


class Proposal:
    """The reviewable outcome of preparing one or more targets.

    A read-only report with an opaque native handle to the reviewed
    candidate. JSON reports cannot be imported as executable proposals, so
    preparation and application must occur in the same process.

    Attributes:
        schema_version: Version of the report format.
        root: Canonical repository root.
        targets: Targets that were prepared.
        changes: Exact file changes that applying writes.
        dependencies: Packages that change, including transitive ones.
        suggestions: Suggestions about declared constraints.
        unresolved: References left unchanged because they could not be resolved.
        failures: Targets that could not be prepared.
        validation: Validation levels completed and acceptance notes.
        scans: Vulnerability scan reports, when scanning was requested.
    """

    def __init__(self, native: _native.NativeProposal):
        """Wrap a native proposal; use :func:`prepare` to create one."""
        self._native = native
        data = json.loads(native.report())
        self.schema_version: int = data["schema_version"]
        self.root: str = data["root"]
        self.targets = tuple(Target(**row) for row in data["targets"])
        self.changes = tuple(FileChange(**row) for row in data["changes"])
        self.suggestions = tuple(
            Suggestion(**{**row, "evidence": tuple(row.get("evidence", ()))})
            for row in data["suggestions"]
        )
        self.unresolved = tuple(Unresolved(**row) for row in data["unresolved"])
        self.failures = tuple(Failure(**row) for row in data["failures"])
        self.validation: tuple[str, ...] = tuple(data["validation"])
        self.scans = tuple(ScanReport._from_dict(row) for row in data["scans"])
        self.dependencies = tuple(
            DependencyChange(
                Package(**row["before"]) if row["before"] else None,
                Package(**row["after"]) if row["after"] else None,
            )
            for row in data["dependencies"]
        )

    def to_dict(self) -> dict[str, Any]:
        """Return the proposal as the same JSON-compatible report the CLI prints."""
        return json.loads(self._native.report())

    def apply(self, *, allow_partial: bool = False) -> ApplyResult:
        r"""Write exactly the reviewed files, without resolving again.

        Args:
            allow_partial: Apply the successful targets of a proposal with
                failures instead of refusing.

        Returns:
            The files written.

        Raises:
            StaleProposalError: The repository changed since preparation.
            OperationError: Targets failed and ``allow_partial`` is false, or
                another operation holds the repository lock.

        Example:
            >>> import pathlib, tempfile, depsmith
            >>> repo = tempfile.mkdtemp()
            >>> _ = pathlib.Path(repo, "pixi.toml").write_text("[workspace]\nname = 'demo'\n")
            >>> options = depsmith.UpdateOptions(pixi="no-such-pixi")
            >>> proposal = depsmith.prepare(repo, targets=["pixi:pixi.toml"], options=options)
            >>> proposal.apply()
            Traceback (most recent call last):
            ...
            depsmith.OperationError: operation failed: some targets failed; explicit partial application required
        """
        data = json.loads(self._native.apply(allow_partial))
        return ApplyResult(
            data["schema_version"], tuple(data["applied"]), data["partial"]
        )


def discover(root: Path = ".") -> tuple[Target, ...]:
    """Find the targets under ``root``, skipping ignored and environment directories.

    Args:
        root: Repository root.

    Returns:
        The discovered targets.
    """
    return tuple(Target(**row) for row in json.loads(_native.discover_json(str(root))))


def prepare(
    root: Path = ".",
    *,
    targets: Sequence[str] = (),
    options: UpdateOptions | None = None,
) -> Proposal:
    r"""Prepare a reviewable proposal for the selected targets.

    Each target is resolved in a stage; the repository is not modified. A
    target that cannot be prepared is recorded in ``Proposal.failures``.

    Args:
        root: Repository root.
        targets: Target identifiers; empty uses the saved selection.
        options: Options overriding the repository settings.

    Returns:
        The proposal to review and apply.

    Raises:
        ConfigurationError: Invalid options, no or unknown targets, or
            selected packages that are not direct dependencies.

    Example:
        >>> import pathlib, tempfile, depsmith
        >>> repo = tempfile.mkdtemp()
        >>> _ = pathlib.Path(repo, "pixi.toml").write_text("[workspace]\nname = 'demo'\n")
        >>> options = depsmith.UpdateOptions(pixi="no-such-pixi")
        >>> proposal = depsmith.prepare(repo, targets=["pixi:pixi.toml"], options=options)
        >>> [f.code for f in proposal.failures]
        [3]
    """
    return Proposal(
        _native.prepare(str(root), list(targets), (options or UpdateOptions())._json())
    )


def scan(
    root: Path = ".",
    *,
    targets: Sequence[str] = (),
    options: UpdateOptions | None = None,
) -> tuple[ScanReport, ...]:
    """Scan the current locks of the selected targets without updating.

    There is no baseline to compare against, so every report is
    ``candidate-only``.

    Args:
        root: Repository root.
        targets: Target identifiers; empty uses the saved selection.
        options: Options; scanning is enabled by default.

    Returns:
        One report per target.

    Raises:
        ConfigurationError: Invalid options, unknown targets, or a target
            without a lock to scan.
        OperationError: The scanner failed.

    Example:
        >>> import tempfile, depsmith
        >>> depsmith.scan(tempfile.mkdtemp(), targets=["missing"])
        Traceback (most recent call last):
        ...
        depsmith.ConfigurationError: invalid request: unknown target: missing
    """
    return tuple(
        ScanReport._from_dict(row)
        for row in json.loads(
            _native.scan_json(
                str(root), list(targets), (options or UpdateOptions(scan=True))._json()
            )
        )
    )


def doctor(root: Path = ".", *, options: UpdateOptions | None = None) -> dict[str, Any]:
    """Report adapter capabilities and whether each native tool is available.

    Args:
        root: Repository root whose settings apply.
        options: Options overriding the repository settings.

    Returns:
        The same report as ``depsmith doctor --json``.
    """
    return json.loads(
        _native.doctor_json(str(root), (options or UpdateOptions())._json())
    )


def init(
    root: Path = ".",
    *,
    fetch_tools: bool = False,
    options: UpdateOptions | None = None,
) -> dict[str, Any]:
    """Check the native tools the targets under ``root`` use.

    Never prompts: a missing used tool is installed (its pinned,
    sha256-verified release, into the tool cache) only with ``fetch_tools``.

    Args:
        root: Repository root to scan.
        fetch_tools: Install every missing used tool that has a download for
            this host.
        options: Options overriding the repository settings; ``scan`` adds
            the scanner to the tools checked.

    Returns:
        The same report as ``depsmith init --json``: ``targets``, ``tools``
        (each with ``used_by``), ``installed`` and ``missing``.
    """
    return json.loads(
        _native.init_json(str(root), (options or UpdateOptions())._json(), fetch_tools)
    )


def recover(root: Path = ".") -> tuple[str, ...]:
    """Restore the files of an interrupted apply from its journal.

    Args:
        root: Repository root.

    Returns:
        The restored paths, relative to the root.

    Raises:
        ConfigurationError: There is no interrupted operation to recover.
        StaleProposalError: A file changed after the interruption.
    """
    return tuple(json.loads(_native.recover_json(str(root))))
