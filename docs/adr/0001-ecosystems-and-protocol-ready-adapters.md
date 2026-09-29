# Package managers are adapters composed from shared ecosystems, behind a protocol-ready seam

Adding package managers easily is depsmith's foremost requirement, so a manager adapter only describes its files, native tool and declarations, while everything tied to an ecosystem (version scheme, registries for availability evidence, upstream identities for scanning) lives in shared ecosystem modules that several managers compose: Pixi uses conda and PyPI, conda-lock uses conda and PyPI, cargo uses crates, and GitHub Actions uses its own. Suggestions, acceptance and scan identities are therefore engine code, not per-adapter code. Adapters are in-tree for now, but every type crossing the adapter seam is owned and serde-serialisable, so an out-of-tree plugin protocol (JSON over a subprocess) can be added later as a bridge without redesigning the seam.

## Considered options

- **Out-of-tree plugins now**: rejected for the first release; a versioned protocol, plugin discovery and trust add more than the current managers need.
- **Protocol-first request/response enums for in-tree adapters**: rejected; weakly typed and wide, and it still leaves constraint logic duplicated per adapter.
- **Keep constraint logic inside each adapter**: rejected; a conda adapter would duplicate about 250 lines of Pixi's logic and a uv adapter about 400.
