# syntax=docker/dockerfile:1.7

FROM rust:1-slim-bookworm AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates pkg-config build-essential \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release --locked --bin firmament

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /app --shell /usr/sbin/nologin firmament \
    && mkdir -p /app /data \
    && chown -R firmament:firmament /app /data

WORKDIR /app

COPY --from=builder /app/target/release/firmament /usr/local/bin/firmament
COPY --chmod=755 docker/solver-entrypoint.sh /usr/local/bin/firmament-solver-entrypoint

USER firmament

EXPOSE 5050

ENTRYPOINT ["firmament-solver-entrypoint"]
CMD ["firmament"]
