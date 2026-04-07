# Stage 1: Build the Rust binary
FROM --platform=linux/amd64 rust:bookworm AS builder

# Create a new empty shell project
WORKDIR /usr/src/app

# Install wget, unzip, and g++ for compiling C++ bindings
RUN apt-get update && apt-get install -y wget unzip g++ && rm -rf /var/lib/apt/lists/*

# Download and extract LibTorch (CPU version 2.1.0 for rust-bert 0.22/tch 0.14)
RUN wget -q https://download.pytorch.org/libtorch/cpu/libtorch-cxx11-abi-shared-with-deps-2.1.0%2Bcpu.zip -O libtorch.zip && \
    unzip -q libtorch.zip -d /opt && \
    rm libtorch.zip

ENV LIBTORCH=/opt/libtorch
ENV LD_LIBRARY_PATH=/opt/libtorch/lib
ENV SQLX_OFFLINE=true

# We need the `.sqlx` directory for offline SQLx compilation
# so that the build doesn't require a live PostgreSQL connection
COPY .sqlx ./.sqlx

# Copy the actual code
COPY src ./src
COPY examples ./examples
COPY Cargo.toml Cargo.lock ./

# Pre-download the HuggingFace model cache so it's baked into the Docker image
RUN cargo run --release --example download_model --features ml

# Build the release binary
RUN cargo build --release --features ml

# Stage 2: Create a minimal runtime environment
FROM --platform=linux/amd64 debian:bookworm-slim

# Install OpenSSL, CA certificates, and libgomp1 (needed by torch)
RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 libgomp1 && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy LibTorch shared libraries
COPY --from=builder /opt/libtorch /opt/libtorch
ENV LD_LIBRARY_PATH=/opt/libtorch/lib

# Copy HuggingFace offline model cache
COPY --from=builder /root/.cache /root/.cache

# Copy the compiled binary from the builder environment
COPY --from=builder /usr/src/app/target/release/matchmaker-orchestrator /app/matchmaker-orchestrator

# Expose the Axum server port
EXPOSE 3000

# Set the entrypoint
CMD ["./matchmaker-orchestrator"]
