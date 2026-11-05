# Development image: Rust toolchain for building and testing aip.
# Includes the MinGW-w64 cross toolchain so `make dist` can also produce a Windows binary.
FROM rust:1.97-slim

RUN rustup component add rustfmt clippy llvm-tools-preview \
    && cargo install cargo-llvm-cov --locked \
    && rm -rf "${CARGO_HOME}/registry" \
    && rustup target add x86_64-pc-windows-gnu \
    && apt-get update \
    && apt-get install -y --no-install-recommends git gcc-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/*

# Run as the host user so files written to the bind-mounted repo are not root-owned.
ARG UID=1000
ARG GID=1000
RUN (getent group "${GID}" || groupadd --gid "${GID}" dev) \
    && (getent passwd "${UID}" || useradd --create-home --uid "${UID}" --gid "${GID}" dev) \
    && mkdir -p /target "${CARGO_HOME}/registry" \
    && chown -R "${UID}:${GID}" /target "${CARGO_HOME}/registry"

ENV CARGO_TARGET_DIR=/target
WORKDIR /workspace
