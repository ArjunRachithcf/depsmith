# conda-forge recipe

`recipe.yaml` is the conda-forge recipe (rattler-build, schema v1) for the
latest release on PyPI: its source is that sdist and its SHA-256. It is what a
staged-recipes submission or the feedstock carries; once the feedstock exists,
the conda-forge bot follows new PyPI releases there. The package contains the
PyO3 extension and the `depsmith` entry point; Pixi, uv, npm and the other
managers are separate tools (`depsmith init` can install them).

- **One build per platform**: the extension uses the stable ABI (abi3), so
  each platform gets a single package for Python 3.10 and newer.
- **Licenses**: `cargo-bundle-licenses` writes the licenses of every bundled
  crate to `THIRDPARTY.yml`, which ships beside `LICENSE`.
- **Release candidates**: `variants.yaml` sends release candidates to the
  `conda-forge/label/depsmith_rc` channel
  (`conda install -c conda-forge/label/depsmith_rc depsmith`). Remove it for a
  final release.

CI builds and tests this recipe on linux-64 with the commit's own sdist in
place of the published one; `.github/conda-variants.yaml` stands in for the
conda-forge pinning keys the recipe uses (CI pins rattler-build in
`.github/workflows/ci.yml`). To build the published release the
same way:

```sh
pixi exec --spec rattler-build==0.76.1 rattler-build build --recipe recipes/depsmith/recipe.yaml \
  -m .github/conda-variants.yaml -c conda-forge --output-dir conda-dist
```

macOS and Windows builds are first checked by the feedstock's CI.
