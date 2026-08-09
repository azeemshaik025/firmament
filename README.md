# Firmament — Solana RFQ Maker Runtime

Firmament keeps a USDC reserve, sources assets through Jupiter, quotes RFQs under
policy, settles on Solana HTLCs, and repairs inventory after each fill.

You run it yourself — the maker wallet, keys, and policy stay under your control.
It's safe by default: the runtime boots with live protocol workers disabled until
you configure real credentials, a funded wallet, and explicit caps.

## What's in the repo

- **Rust crate** — the maker runtime and Axum HTTP API, with SQLite persistence.
- **`web/app`** — a minimal Vite/React surface for quotes, wallet settlement, and read-only operator stats.
- **`landing`** — a Next.js landing page (the only piece meant to be deployed publicly).
- Adapters for Solana wallets/HTLCs, Jupiter Swap API V2, and Circle Gateway.

## Quickstart

Prerequisites: Rust 1.85+ (edition 2024), Node.js 18+, and a Solana RPC endpoint
for anything beyond offline mode.

```bash
git clone https://github.com/azeemshaik025/firmament.git
cd firmament

# Backend API/runtime → 127.0.0.1:5050
cp .env.example .env
cp config.example.toml config.toml
cargo run

# Frontend (second terminal) → http://127.0.0.1:3000/app
npm run dev

# Landing page (optional) → http://127.0.0.1:3001
npm run landing:dev
```

The frontend proxies `/v1` requests to the backend. Keep `cargo run` and
`npm run dev` in separate terminals — the backend does not serve frontend files.

## Configuration

Secrets go in `.env`; policy and non-secret settings live in `config.toml`. Set
exactly one maker keypair source and verify asset mints before enabling live
workers. The sample config ships with tiny safety caps and protocol workers off.

Full setup — wallets, live mode, caps, cbBTC mint — is in
[docs/maker-setup.mdx](docs/maker-setup.mdx).

## Tests

```bash
cargo test              # safe default; live tests skip unless RUN_LIVE_* is set
scripts/smoke.sh all-non-mutating
```

Mutating smoke groups are real mainnet actions — read `scripts/smoke.sh` and set
the opt-in env vars first.

## License

Dual-licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
