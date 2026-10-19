# axiond in a container: one static binary, nothing else.
#   docker build -f crates/axiond/Dockerfile -t axiond .        (from the repo root)
#   docker run -d --name axiond -p 127.0.0.1:8065:8065 -v axiond:/data \
#     -e AXIOND_BEHIND_PROXY=true axiond
#   docker exec -it axiond axiond setup --admin you --email you@example.com --team home
# axiond doesn't do TLS yet: put Caddy or another TLS proxy in front.

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p axiond && cp target/release/axiond /axiond
# An empty /data the server's user owns.
RUN mkdir -p /data && chown 65532:65532 /data

FROM scratch
COPY --from=build /axiond /usr/local/bin/axiond
COPY --from=build --chown=65532:65532 /data /data
USER 65532:65532
ENV AXIOND_LISTEN=0.0.0.0:8065 AXIOND_DATA_DIR=/data
VOLUME /data
EXPOSE 8065
HEALTHCHECK --interval=30s --timeout=5s CMD ["axiond", "ping"]
ENTRYPOINT ["axiond"]
