# The sandbox ai-compete runs contenders and their gates in (see .compete.toml).
# It is built with an empty context and runs as the invoking user (--userns=keep-id).
FROM docker.io/library/rust:1.98-bookworm

RUN apt-get update \
 && apt-get install -y --no-install-recommends shellcheck libpython3-dev \
 && rm -rf /var/lib/apt/lists/* \
 && rustup component add rustfmt clippy

COPY --from=ghcr.io/astral-sh/uv:0.12.15 /uv /uvx /usr/local/bin/
ENV UV_PYTHON_INSTALL_DIR=/opt/uv-python \
    UV_TOOL_DIR=/opt/uv-tools \
    UV_TOOL_BIN_DIR=/usr/local/bin
RUN uv python install 3.10 \
 && uv tool install prek==0.5.4 \
 && chmod -R a+rX /opt/uv-python /opt/uv-tools

# The native Claude Code binary is self-contained; copy it out of root's home.
RUN curl -fsSL https://claude.ai/install.sh | bash \
 && cp -L /root/.local/bin/claude /usr/local/bin/claude \
 && chmod a+rx /usr/local/bin/claude

# Cargo's registry and git caches are bind-mounted under the contender's home;
# the toolchain itself stays read-only in /usr/local.
ENV CARGO_HOME=/home/agent/.cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    DISABLE_AUTOUPDATER=1
