# Build stage
FROM rust:bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release

# Runtime stage — API key is NOT baked into the image; injected at run time via env_file / secrets.
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/mcp-redmine .

ENV RUST_LOG=info
ENV MCP_BIND=0.0.0.0
ENV MCP_PORT=8080
EXPOSE 8080

# SSE MCP server. API key is injected at runtime via env_file — never baked into the image.
ENTRYPOINT ["./mcp-redmine"]