# Digest-pinned multi-stage production image for DT self-hostable server & React web client.
FROM rust:1.99.1-bookworm@sha256:0000000000000000000000000000000000000000000000000000000000000000 AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p gm-daemon --bin dt --bin gm-daemon --bin dt-doctor

FROM debian:bookworm-slim@sha256:0000000000000000000000000000000000000000000000000000000000000000
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 libsqlite3-0 && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --create-home dt
COPY --from=builder /src/target/release/dt /usr/local/bin/dt
COPY --from=builder /src/target/release/gm-daemon /usr/local/bin/gm-daemon
COPY --from=builder /src/target/release/dt-doctor /usr/local/bin/dt-doctor
USER dt
EXPOSE 7433
HEALTHCHECK --interval=15s --timeout=5s --retries=3 CMD ["/usr/local/bin/dt-doctor", "check", "--json"]
ENTRYPOINT ["/usr/local/bin/gm-daemon", "serve", "--bind", "127.0.0.1:7433"]
