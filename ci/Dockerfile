# The Entroq integration environment.
#
# The image holds the host tools a validation campaign needs, and no source.
# A campaign mounts the repository read only, so a run in this container
# cannot write inside the repository.
#
# The image installs rustup with no toolchain of its own, then installs the
# toolchain that rust-toolchain.toml names. No version appears in this file.
# The toolchain version keeps one owner.
#
# The base image is pinned by digest. Moving it is a deliberate change, not a
# rebuild side effect.

FROM debian:bookworm-slim@sha256:88200866dfff7ea7f5cbcb6ec7c8a701889efe6fe859fe64d6990e4b07ea4171

# `git` reports the revision and the input set. `build-essential` provides the
# linker the toolchain invokes. `make` is the entry point a lane calls.
RUN apt-get update \
    && apt-get install --no-install-recommends --yes \
        build-essential \
        ca-certificates \
        curl \
        git \
        make \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN curl --proto '=https' --tlsv1.2 --fail --silent --show-error https://sh.rustup.rs \
    | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none

# The repository is created by another user, so git needs the mount declared.
RUN git config --system --add safe.directory /work

# `rustup install` with no argument installs the toolchain the file resolves to,
# and the copy makes the file the only input that can change which one that is.
COPY rust-toolchain.toml /opt/toolchain/
WORKDIR /opt/toolchain
RUN rustup install

WORKDIR /work
