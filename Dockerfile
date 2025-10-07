# ==========================
# Builder
# ==========================
FROM rust:1.80-slim AS builder

WORKDIR /app

# Install build deps for sqlx/sqlite and ring
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev ca-certificates build-essential \
    && rm -rf /var/lib/apt/lists/*

# Cache deps
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "fn main(){}" > src/main.rs
RUN cargo build --release || true

# Copy source
COPY . .
RUN cargo build --release

# ==========================
# Runtime
# ==========================
FROM debian:bookworm-slim AS runtime

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates openssl sqlite3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/benny-rs /app/benny-rs

# Create directories for data/logs
RUN mkdir -p /app/databases /app/logs

# The bot reads config.json at runtime; mount it as a volume/secret
VOLUME ["/app/databases", "/app/logs"]

ENV RUST_LOG=info

CMD ["/app/benny-rs"]


