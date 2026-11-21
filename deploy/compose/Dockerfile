FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p globalobject-control-plane -p globalobject-node -p globalobject-client

FROM debian:bookworm-slim
COPY --from=build /src/target/release/globalobject-control-plane /usr/local/bin/
COPY --from=build /src/target/release/globalobject-node /usr/local/bin/
COPY --from=build /src/target/release/globalobject /usr/local/bin/
