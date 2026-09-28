# Local conda recipe

Build from this checkout using `conda build -c conda-forge recipes/depsmith`.
The package contains the PyO3 extension and `depsmith` entry point. Pixi and
Grype are separate runtime tools. This recipe is not yet tested with conda-build.

Before conda-forge submission, replace the local source path with an immutable
release archive and SHA-256, add the actual project homepage and feedstock
maintainers, and vendor Cargo dependencies for offline reproducibility. Confirm
all target platforms with feedstock CI. No package publication is configured.

Recipe fields follow the [conda-build metadata documentation](https://docs.conda.io/projects/conda-build/en/latest/resources/define-metadata.html).
