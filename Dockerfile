# ---- build ----
FROM rust:1-slim AS build
WORKDIR /src

# Cache dependency compilation separately from the source.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- runtime ----
FROM debian:bookworm-slim
COPY --from=build /src/target/release/smart-meter-brute-force-pin /usr/local/bin/smart-meter-brute-force-pin

# Checkpoint lives here so progress survives a container restart.
# Mount a named volume at /data to persist it across `docker run`s.
VOLUME ["/data"]

CMD ["smart-meter-brute-force-pin"]
