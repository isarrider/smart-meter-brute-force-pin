# ---- build ----
# Cross-compile a static musl binary for the target arch on the native build
# platform (no QEMU). cargo-zigbuild uses zig as a universal cross-linker, so
# amd64/arm64/armv7/riscv64 all build the same way. This also covers arches
# that have no official rust:alpine image (riscv64, armv7).
FROM --platform=$BUILDPLATFORM rust:1-alpine AS build
WORKDIR /src
ARG TARGETPLATFORM

RUN apk add --no-cache musl-dev py3-pip \
 && pip install --break-system-packages ziglang \
 && cargo install cargo-zigbuild --locked

COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN set -eux; \
    case "$TARGETPLATFORM" in \
      "linux/amd64")   TARGET=x86_64-unknown-linux-musl ;; \
      "linux/arm64")   TARGET=aarch64-unknown-linux-musl ;; \
      "linux/arm/v7")  TARGET=armv7-unknown-linux-musleabihf ;; \
      "linux/riscv64") TARGET=riscv64gc-unknown-linux-musl ;; \
      *) echo "unsupported platform: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    rustup target add "$TARGET"; \
    cargo zigbuild --release --target "$TARGET"; \
    install -D "target/$TARGET/release/smart-meter-brute-force-pin" /out/smart-meter-brute-force-pin

# ---- runtime ----
# The final stage only COPYs the static binary (no RUN), so buildx assembles the
# per-arch image without emulating the target.
FROM alpine:3.20
COPY --from=build /out/smart-meter-brute-force-pin /usr/local/bin/smart-meter-brute-force-pin

# Checkpoint lives here so progress survives a container restart.
# Mount a named volume at /data to persist it across `docker run`s.
VOLUME ["/data"]

CMD ["smart-meter-brute-force-pin"]
