# Stage 1: Build the Rust binary
FROM rust:bookworm AS builder

# Create a new empty shell project
WORKDIR /usr/src/app

# Install wget, unzip (no longer strictly needed for C++ ML bindings, but kept for general usefulness)
RUN apt-get update && apt-get install -y wget unzip && rm -rf /var/lib/apt/lists/*

ENV SQLX_OFFLINE=true

# We need the `.sqlx` directory for offline SQLx compilation
# so that the build doesn't require a live PostgreSQL connection
COPY .sqlx ./.sqlx

# Copy dependency files first
COPY Cargo.toml Cargo.lock ./

# Create dummy source files to trigger dependency compilation
RUN mkdir src examples && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/lib.rs && \
    echo "fn main() {}" > examples/download_model.rs && \
    echo "fn main() {}" > examples/load_test.rs

# Compile dependencies (this step will be cached)
RUN cargo build --release

# Remove dummy files
RUN rm -rf src examples

# Copy the actual code
COPY src ./src
COPY examples ./examples

# Touch the main files to ensure cargo rebuilds the updated code instead of using the dummy binaries
RUN touch src/main.rs src/lib.rs examples/download_model.rs examples/load_test.rs

# Build the release binary
RUN cargo build --release

# Stage 2: Create a minimal runtime environment
FROM debian:bookworm-slim

# Install OpenSSL and CA certificates (libgomp1 no longer needed)
RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the compiled binary from the builder environment
COPY --from=builder /usr/src/app/target/release/matchmaker-orchestrator /app/matchmaker-orchestrator

# Expose the Axum server port
EXPOSE 3000

# Set the entrypoint
CMD ["./matchmaker-orchestrator"]
