# syntax=docker/dockerfile:1
#
# wheezper-server only. The workspace also holds the CLI, but this image does
# not build whisper.cpp/llama.cpp: `cargo build -p wheezper-server` skips them.
#
#   docker build -f server/Dockerfile -t wheezper-server .

FROM rust:1-bookworm AS build
WORKDIR /src

# Manifest resolution needs every workspace member and the [patch] paths.
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
COPY llm-worker ./llm-worker
COPY vendor ./vendor
COPY proto ./proto
COPY server ./server

RUN cargo build --release -p wheezper-server --bins \
    && strip target/release/wheezper-server target/release/wheezper-admin

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/wheezper-server /usr/local/bin/wheezper-server
# Operator CLI (pair devices, create sessions, read transcripts) in the same image.
COPY --from=build /src/target/release/wheezper-admin /usr/local/bin/wheezper-admin
# Run unprivileged; the database is the only state this process owns.
USER 10001
EXPOSE 50051
ENTRYPOINT ["wheezper-server"]
