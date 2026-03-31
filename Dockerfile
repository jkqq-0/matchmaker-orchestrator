# Stage 1: Build the Rust binary
FROM rust:bookworm AS builder

# Create a new empty shell project
WORKDIR /usr/src/app

# We need the `.sqlx` directory for offline SQLx compilation
# so that the build doesn't require a live PostgreSQL connection
COPY .sqlx ./.sqlx

# Copy the actual code
COPY src ./src
COPY Cargo.toml Cargo.lock ./

# Ensure SQLx uses the offline metadata stored in .sqlx
ENV SQLX_OFFLINE=true

# Build the release binary
RUN cargo build --release

# Stage 2: Create a minimal runtime environment
FROM debian:bookworm-slim

# Install OpenSSL and CA certificates needed for HTTPS requests (e.g. AWS S3, OpenAI)
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
