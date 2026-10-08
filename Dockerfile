# Multi-stage Dockerfile for 9router Telegram Bot
FROM rust:1-slim-bookworm AS builder

WORKDIR /app

# Cache dependency layer
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src

# Copy real source code
COPY src ./src

# Build the real binary
RUN touch src/main.rs && cargo build --release --locked

# Final runtime image
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as non-root user
RUN useradd -m -u 1000 -U appuser
USER appuser

WORKDIR /app

COPY --from=builder /app/target/release/nine_router_bot /usr/local/bin/nine_router_bot

ENV RUST_LOG="info,nine_router_bot=debug"

ENTRYPOINT ["/usr/local/bin/nine_router_bot"]
