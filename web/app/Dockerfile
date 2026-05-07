# syntax=docker/dockerfile:1.7

# Build from the repository root:
#   docker build -t firmament-app web/app
FROM node:22-alpine AS deps

WORKDIR /app

COPY package.json package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci

FROM node:22-alpine AS build

WORKDIR /app

COPY --from=deps /app/node_modules ./node_modules
COPY . .

ARG VITE_SOLANA_RPC_URL=https://api.mainnet-beta.solana.com
ENV VITE_SOLANA_RPC_URL=${VITE_SOLANA_RPC_URL}

RUN npm run build

FROM caddy:2.9-alpine AS runtime

ENV FIRMAMENT_BACKEND_ORIGIN=http://firmament-solver:5050
ENV FIRMAMENT_SOLANA_RPC_URL=https://api.mainnet-beta.solana.com

RUN addgroup -S firmament \
    && adduser -S firmament -G firmament \
    && mkdir -p /srv/app /data /config \
    && chown -R firmament:firmament /srv /data /config

COPY Caddyfile /etc/caddy/Caddyfile
COPY --chmod=755 docker-entrypoint.sh /usr/local/bin/firmament-app-entrypoint
COPY --from=build --chown=firmament:firmament /app/dist /srv/app

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://127.0.0.1:8080/health >/dev/null || exit 1

USER firmament
ENTRYPOINT ["firmament-app-entrypoint"]
CMD ["caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]
