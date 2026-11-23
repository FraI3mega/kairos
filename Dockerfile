# Builds the server from source, for hosts that build straight from the repo. deploy/Dockerfile is the other way:
# it packages a binary built on the host.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p kairos-server

FROM debian:bookworm-slim
RUN useradd -r -u 10001 kairos
COPY --from=build /src/target/release/kairos-server /usr/local/bin/kairos-server
USER kairos
ENV BIND=0.0.0.0:8787
EXPOSE 8787
CMD ["kairos-server"]
