# TideDesk rendezvous service: introduces viewers to hosts, never carries sessions.
# Build from the repository root:  docker build -f deploy/Dockerfile -t tidedesk-rendezvous .
# Run:  docker run -d --restart=always -p 47900-47901:47900-47901/udp tidedesk-rendezvous

FROM rust:1.88-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p tidedesk-rendezvous

FROM debian:bookworm-slim
COPY --from=build /src/target/release/tidedesk-rendezvous /usr/local/bin/tidedesk-rendezvous
USER nobody
EXPOSE 47900/udp 47901/udp
ENTRYPOINT ["tidedesk-rendezvous"]
