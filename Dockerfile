# ---- build ----
FROM rust:1.85-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY ui ./ui
# cache deps first (rebuilt only when Cargo files change)
RUN cargo build --release

# ---- run ----
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /app/target/release/llm2 /usr/local/bin/llm-proxy
COPY config.example.json ./config.example.json
ENV PORT=3000 HOST=0.0.0.0 CONFIG_PATH=/app/config.json RUST_LOG=llm2=info
EXPOSE 3000
VOLUME ["/app"]
ENTRYPOINT ["/usr/local/bin/llm-proxy"]
