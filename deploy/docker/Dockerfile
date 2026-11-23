# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS rust-builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang pkg-config libssl-dev libsqlite3-dev libpipewire-0.3-dev libspa-0.2-dev \
    libxkbcommon-dev \
    libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src/DT
COPY . .
RUN cargo build --release --locked -p gm-daemon --bin dt --bin gm --bin gm-daemon --bin dt-doctor

FROM node:22-bookworm-slim@sha256:e9f565d32859f0cbfb52125eafe8be39390f24dac302dc3cdf566e8ba3ab21ee AS web-builder
WORKDIR /web
COPY clients/web/package.json clients/web/package-lock.json ./
RUN npm ci
COPY clients/web/ ./
RUN npm run typecheck && npm run build

FROM caddy:2.9-alpine@sha256:b4e3952384eb9524a887633ce65c752dd7c71314d2c2acf98cd5c715aaa534f0 AS tls-proxy
COPY deploy/docker/Caddyfile /etc/caddy/Caddyfile

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS dt-server
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl libssl3 libsqlite3-0 libpipewire-0.3-0 libatspi2.0-0 \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 --create-home dt && install -d -o dt -g dt -m 0700 /var/lib/dt
COPY --from=rust-builder /src/DT/target/release/dt /usr/local/bin/dt
COPY --from=rust-builder /src/DT/target/release/gm /usr/local/bin/gm
COPY --from=rust-builder /src/DT/target/release/gm-daemon /usr/local/bin/gm-daemon
COPY --from=rust-builder /src/DT/target/release/dt-doctor /usr/local/bin/dt-doctor
COPY --from=web-builder /web/dist /usr/share/dt/web
ENV XDG_DATA_HOME=/var/lib/dt
ENV DT_WEB_DIST=/usr/share/dt/web
USER dt
HEALTHCHECK --interval=15s --timeout=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:7433/api/v1/health/ready >/dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/gm-daemon", "serve", "--bind", "127.0.0.1:7433"]
