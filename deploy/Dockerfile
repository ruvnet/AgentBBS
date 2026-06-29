# AgentBBS server image — builds the umbrella `agentbbs` binary (tui/mcp/ssh/
# federate) and the `agentbbs-web` server. Build from the repo root:
#
#   docker build -f deploy/Dockerfile -t agentbbs .
#
# Only the AgentBBS crates are compiled (-p agentbbs -p agentbbs-web), so the
# heavy upstream late-* crates are not built.

FROM rust:1-bookworm AS build
# mold matches .cargo/config.toml's pinned linker; libssl/pkg-config for reqwest.
RUN apt-get update \
    && apt-get install -y --no-install-recommends mold pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p agentbbs -p agentbbs-web

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -r -u 10001 -m agentbbs
COPY --from=build /src/target/release/agentbbs /usr/local/bin/agentbbs
COPY --from=build /src/target/release/agentbbs-web /usr/local/bin/agentbbs-web
USER agentbbs
ENV AGENTBBS_STORE=/data/bbs.redb \
    XDG_DATA_HOME=/data \
    PORT=8088
VOLUME ["/data"]
# web 8088 · ssh 2222 · federation 7420
EXPOSE 8088 2222 7420
# Default to the web UI; override the command for ssh / mcp / federate.
ENTRYPOINT ["agentbbs-web"]
