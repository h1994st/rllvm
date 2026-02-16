# syntax=docker/dockerfile:1

# ==============================================================================
# Build stage: compile rllvm from source
# ==============================================================================
FROM rust:latest AS builder

WORKDIR /usr/src/rllvm
COPY . .

RUN cargo build --release \
    && strip target/release/rllvm-cc \
             target/release/rllvm-cxx \
             target/release/rllvm-get-bc \
             target/release/rllvm-init \
             target/release/rllvm-info \
             target/release/rllvm-rustc \
             target/release/rllvm-completions

# ==============================================================================
# Runtime stage: slim image with LLVM toolchain
# ==============================================================================
FROM debian:bookworm-slim

# Install LLVM/Clang and minimal runtime dependencies
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        llvm \
        clang \
        llvm-dev \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy rllvm binaries from the build stage
COPY --from=builder /usr/src/rllvm/target/release/rllvm-cc       /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-cxx      /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-get-bc   /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-init      /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-info      /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-rustc     /usr/local/bin/
COPY --from=builder /usr/src/rllvm/target/release/rllvm-completions /usr/local/bin/

# Verify installation
RUN rllvm-cc --help > /dev/null 2>&1

WORKDIR /workspace

ENTRYPOINT ["/bin/bash"]
