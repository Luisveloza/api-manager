# Multi-stage Dockerfile for APIManager headless mode
# Build stage: Compile the Rust binary
FROM rust:1.82-bookworm AS builder

WORKDIR /build

# Copy Cargo files for dependency caching
COPY src-tauri/Cargo.toml src-tauri/Cargo.lock* ./src-tauri/
COPY src-tauri/build.rs ./src-tauri/
COPY src-tauri/src/ ./src-tauri/src/
COPY src-tauri/icons/ ./src-tauri/icons/
COPY src-tauri/tauri.conf.json ./src-tauri/

# Create dist placeholder for tauri build
RUN mkdir -p dist && echo '<html><body>APIManager Headless</body></html>' > dist/index.html

# Build the binary (release mode, headless only)
WORKDIR /build/src-tauri
RUN cargo build --release --bin apimanager

# Runtime stage: Minimal image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/src-tauri/target/release/apimanager /usr/local/bin/apimanager

# Create config directory
RUN mkdir -p /root/.config/APIManager

EXPOSE 8045

ENV PORT=8045
ENV ABV_AUTH_MODE=all_except_health

ENTRYPOINT ["apimanager", "--headless"]
