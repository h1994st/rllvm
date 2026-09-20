# syntax=docker/dockerfile:1

# ==============================================================================
# Build stage: compile rllvm from source
# ==============================================================================
FROM rust:latest AS builder

WORKDIR /usr/src/rllvm
COPY . .

# The workspace's default members are rllvm-core and rllvm, which is every
# wrapper binary. rllvm-query is deliberately not here: it links LLVM
# statically through llvm-sys, which needs the LLVM development packages and a
# long build, and it ships as its own crate and formula.
RUN cargo build --release \
    && strip target/release/rllvm-cc \
             target/release/rllvm-cxx \
             target/release/rllvm-get-bc \
             target/release/rllvm-compdb \
             target/release/rllvm-init \
             target/release/rllvm-info \
             target/release/rllvm-rustc \
             target/release/rllvm-completions

# ==============================================================================
# Runtime stage: LLVM toolchain plus the wrapper binaries
# ==============================================================================
FROM ubuntu:24.04

# The LLVM the wrappers drive, defaulting to what CI pins. A bitcode reader
# understands its own major and older, never newer, so match this to whatever
# will read the bitcode -- an analyser pinned to an older LLVM cannot read what
# a newer one emits:
#
#   docker build --build-arg LLVM_VERSION=22 -t rllvm .
ARG LLVM_VERSION=23
ENV DEBIAN_FRONTEND=noninteractive

# Ubuntu ships an LLVM far older than the one rustc and current clang emit for,
# so the toolchain comes from apt.llvm.org rather than the distro. `lld` covers
# the LLD, WebAssembly and cross-compilation flows; the build tools are here
# because a container pointed at a real project needs to build it.
RUN apt-get update \
    && apt-get install -y --no-install-recommends gnupg ca-certificates wget \
    && wget -qO /etc/apt/trusted.gpg.d/llvm.asc https://apt.llvm.org/llvm-snapshot.gpg.key \
    && echo "deb http://apt.llvm.org/noble/ llvm-toolchain-noble-${LLVM_VERSION} main" \
        > /etc/apt/sources.list.d/llvm-${LLVM_VERSION}.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        clang-${LLVM_VERSION} \
        llvm-${LLVM_VERSION} \
        llvm-${LLVM_VERSION}-dev \
        lld-${LLVM_VERSION} \
        build-essential \
        cmake \
        ninja-build \
        git \
        python3 \
    && rm -rf /var/lib/apt/lists/*

# Unsuffixed names, so `clang`, `llvm-config` and `ld.lld` resolve without the
# version suffix the apt.llvm.org packages carry.
ENV PATH=/usr/lib/llvm-${LLVM_VERSION}/bin:$PATH

COPY --from=builder /usr/src/rllvm/target/release/rllvm-cc          /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-cxx         /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-get-bc      /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-compdb      /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-init        /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-info        /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-rustc       /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-completions /usr/local/bin/

# Write the config at build time rather than leaving it to first run, so the
# image records `llvm_objcopy_filepath`. Without it embedding falls back to
# rebuilding the object through the `object` crate, which does not model every
# target -- a RISC-V cross build fails on it.
RUN rllvm-init --llvm-prefix /usr/lib/llvm-${LLVM_VERSION} \
    && grep -q llvm_objcopy_filepath /root/.rllvm/config.toml \
    && rllvm-cc --rllvm-version > /dev/null

WORKDIR /workspace

ENTRYPOINT ["/bin/bash"]
