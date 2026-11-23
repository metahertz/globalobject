FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p globalobject-control-plane -p globalobject-node -p globalobject-client

FROM debian:bookworm-slim
# curl is for the health check.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data globalobject && mkdir -p /data && chown globalobject /data
COPY --from=build /src/target/release/globalobject-control-plane /usr/local/bin/
COPY --from=build /src/target/release/globalobject-node /usr/local/bin/
COPY --from=build /src/target/release/globalobject /usr/local/bin/
USER globalobject
WORKDIR /data
EXPOSE 7400
HEALTHCHECK --interval=10s --timeout=3s --retries=6 CMD curl -fsS http://127.0.0.1:7400/readyz || exit 1
CMD ["globalobject-control-plane", "--listen", "0.0.0.0:7400", "--key-file", "/data/control-plane.key"]
