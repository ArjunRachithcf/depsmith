# The sandbox ai-compete runs contenders and their gates in (see .compete.toml).
# It is built with an empty context, runs as the invoking user (--userns=keep-id),
# and is rebuilt weekly with --pull=newer, so it keeps up with CI's toolchains.

# Rust stable, as CI's dtolnay/rust-toolchain@stable; a fixed minor would fall
# behind and let new clippy lints through the gate.
FROM docker.io/library/rust:1-bookworm

RUN rustup component add rustfmt clippy

# shellcheck at the prek hook's version; prek's own hook needs Docker.
RUN curl -fsSL https://github.com/koalaman/shellcheck/releases/download/v0.11.0/shellcheck-v0.11.0.linux.x86_64.tar.xz \
    | tar -xJ --strip-components=1 -C /usr/local/bin shellcheck-v0.11.0/shellcheck

COPY --from=ghcr.io/astral-sh/uv:0.12.15 /uv /uvx /usr/local/bin/
ENV UV_PYTHON_INSTALL_DIR=/opt/uv-python \
    UV_TOOL_DIR=/opt/uv-tools \
    UV_TOOL_BIN_DIR=/usr/local/bin
# Python 3.10 as in CI, also for pyo3, instead of Debian's 3.11.
RUN uv python install 3.10 \
 && ln -s "$(uv python find 3.10)" /usr/local/bin/python3.10 \
 && uv tool install prek==0.5.4 \
 && chmod -R a+rX /opt/uv-python /opt/uv-tools

# The native Claude Code binary is self-contained; copy it out of root's home.
RUN curl -fsSL https://claude.ai/install.sh | bash -s 2.1.294 \
 && cp -L /root/.local/bin/claude /usr/local/bin/claude \
 && chmod a+rx /usr/local/bin/claude

# Cargo's registry and git caches are bind-mounted under the contender's home;
# the toolchain itself stays read-only in /usr/local.
ENV CARGO_HOME=/home/agent/.cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    PYO3_PYTHON=/usr/local/bin/python3.10 \
    DISABLE_AUTOUPDATER=1
