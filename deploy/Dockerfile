# Cloud Run image for agentbbs-web.
#
# agentbbs-web embeds its web assets at compile time via include_str! (see
# crates/agentbbs-web/src/lib.rs), so the runtime image needs only the binary +
# TLS libs — no static files to ship. The server binds 0.0.0.0:$PORT (Cloud Run
# sets PORT). Secrets (the cog_ gateway key) are injected as env at deploy time
# via `gcloud run deploy --set-secrets` — never baked into the image.
#
# Build: docker build -f deploy/Dockerfile -t <img> .   (context = repo root)

# ---- builder ----
FROM rust:slim-bookworm AS builder
# pkg-config + libssl-dev: reqwest's default-tls links system OpenSSL.
# clang + mold: the repo .cargo/config.toml forces `-fuse-ld=mold`.
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev clang mold ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release -p agentbbs-web

# ---- runtime ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --user-group app
COPY --from=builder /app/target/release/agentbbs-web /usr/local/bin/agentbbs-web
USER app
ENV RUST_LOG=info
# Cloud Run provides $PORT; main.rs reads it (default 8088). No EXPOSE needed.
CMD ["/usr/local/bin/agentbbs-web"]
