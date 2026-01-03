# Multi-stage Dockerfile for building Ploy trading bot
# Stage 1: Build environment
FROM debian:bookworm-slim AS builder

# Install Rust via rustup (gets latest stable)
RUN apt-get update && apt-get install -y \
    curl \
    build-essential \
    pkg-config \
    libssl-dev \
    libpq-dev \
    && rm -rf /var/lib/apt/lists/* \
    && curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"

# Create app directory
WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./

# Create dummy main.rs to cache dependencies
RUN mkdir -p src && \
    echo "fn main() {}" > src/main.rs && \
    echo "pub fn lib() {}" > src/lib.rs

# Build dependencies (this layer is cached)
# Note: Don't use --features here since dummy source doesn't define them
RUN cargo build --release 2>/dev/null || true && \
    rm -rf src target/release/deps/ploy* target/release/libploy* target/release/ploy*

# Copy actual source code
COPY src ./src
COPY migrations ./migrations

# Build the actual binary
RUN cargo build --release --features rl

# Stage 2: Runtime environment
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libpq5 \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -r -m -s /bin/bash ploy

# Create directories
RUN mkdir -p /opt/ploy/{bin,config,data,logs,models} && \
    chown -R ploy:ploy /opt/ploy

# Copy binary from builder
COPY --from=builder /app/target/release/ploy /opt/ploy/bin/ploy
RUN chmod +x /opt/ploy/bin/ploy

# Copy config
COPY config/default.toml /opt/ploy/config/default.toml

# Set working directory
WORKDIR /opt/ploy

# Switch to non-root user
USER ploy

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Default command
ENTRYPOINT ["/opt/ploy/bin/ploy"]
CMD ["run"]
