# Local conda recipe

Build from this checkout using `conda build -c conda-forge recipes/depsmith`.
The package contains the PyO3 extension and `depsmith` entry point. Pixi and
Grype are separate runtime tools. Linux build and installation were verified with conda-build on 2026-09-29,
including the Python API tests. CI repeats the Linux build/test; macOS and
Windows conda builds remain unverified. Native Pixi/Grype integrations are
separate opt-in tests.

Before conda-forge submission, replace the local source path with an immutable
release archive and SHA-256, agree on feedstock maintainers, and vendor Cargo dependencies for offline reproducibility. Confirm
all target platforms with feedstock CI. No package publication is configured.

Recipe fields follow the [conda-build metadata documentation](https://docs.conda.io/projects/conda-build/en/latest/resources/define-metadata.html).
