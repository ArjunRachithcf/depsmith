"""Typed Python interface to the same Rust engine used by depsmith."""

from __future__ import annotations

import json
from collections.abc import Sequence
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


@dataclass(frozen=True)
class Target:
    id: str
    manager: str
    manifest: str


@dataclass(frozen=True)
class Suppression:
    """A documented, scoped exception for one advisory; kept in reports, excluded from gates."""

    id: str
    reason: str
    package: str | None = None
    """``ecosystem:name``, e.g. ``pypi:urllib3``."""
    target: str | None = None
    expires: str | None = None
    """Last day (``YYYY-MM-DD``, UTC) the suppression applies."""


@dataclass(frozen=True)
class IdentityMapping:
    ecosystem: str
    name: str
    purl: str
    evidence: str


@dataclass(frozen=True)
class UpdateOptions:
    """None inherits repository settings; explicit values override them."""

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
    ecosystem: str
    name: str
    version: str
    artifact: str
    platform: str


@dataclass(frozen=True)
class DependencyChange:
    before: Package | None
    after: Package | None


@dataclass(frozen=True)
class FileChange:
    target: str
    path: str
    before: str | None
    after: str
    diff: str


@dataclass(frozen=True)
class Suggestion:
    target: str
    package: str
    requirement: str
    reason: str
    evidence: tuple[str, ...] = ()


@dataclass(frozen=True)
class Unresolved:
    """A reference left unchanged because it could not be resolved."""

    target: str
    package: str
    reference: str
    reason: str


@dataclass(frozen=True)
class Failure:
    target: str
    message: str
    code: int


@dataclass(frozen=True)
class Finding:
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
    target: str
    baseline_available: bool
    comparison: str
    """``baseline``, or ``candidate-only`` when no baseline lock exists."""
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
    schema_version: int
    applied: tuple[str, ...]
    partial: bool


class Proposal:
    """Read-only report with an opaque native handle to the reviewed candidate.

    JSON reports cannot be imported as executable proposals. Preparation and
    application must occur in the same process.
    """

    def __init__(self, native: _native.NativeProposal):
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
        return json.loads(self._native.report())

    def apply(self, *, allow_partial: bool = False) -> ApplyResult:
        data = json.loads(self._native.apply(allow_partial))
        return ApplyResult(
            data["schema_version"], tuple(data["applied"]), data["partial"]
        )


def discover(root: Path = ".") -> tuple[Target, ...]:
    return tuple(Target(**row) for row in json.loads(_native.discover_json(str(root))))


def prepare(
    root: Path = ".",
    *,
    targets: Sequence[str] = (),
    options: UpdateOptions | None = None,
) -> Proposal:
    return Proposal(
        _native.prepare(str(root), list(targets), (options or UpdateOptions())._json())
    )


def scan(
    root: Path = ".",
    *,
    targets: Sequence[str] = (),
    options: UpdateOptions | None = None,
) -> tuple[ScanReport, ...]:
    return tuple(
        ScanReport._from_dict(row)
        for row in json.loads(
            _native.scan_json(
                str(root), list(targets), (options or UpdateOptions(scan=True))._json()
            )
        )
    )


def doctor(root: Path = ".", *, options: UpdateOptions | None = None) -> dict[str, Any]:
    return json.loads(
        _native.doctor_json(str(root), (options or UpdateOptions())._json())
    )


def recover(root: Path = ".") -> tuple[str, ...]:
    return tuple(json.loads(_native.recover_json(str(root))))
