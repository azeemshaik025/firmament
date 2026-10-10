# Firmament — firm FX for stablecoin payments on Solana

The rate you promise is the rate they get. A payment app asks Firmament for an
exact recipient amount in EURC, gets the USDC cost held for 30 seconds, and
settles both legs atomically over Solana HTLCs, to the sender or to a named
recipient. Underneath is a self-hosted maker runtime: policy-gated RFQs,
USDC/EURC/SOL working inventory, Jupiter rebalancing, Circle Gateway USDC
refill, and a double-entry ledger.

Why a maker and not a router: on Solana, USDC has about $465M of DEX liquidity
and EURC about $0.7M (Jupiter, October 2026). An app that shows a recipient a
euro amount and then routes through an AMM is guessing. A maker that holds the
euro side quotes a number it can deliver.

You run it yourself — the maker wallet, keys, and policy stay under your control.
It's safe by default: the runtime boots with live protocol workers disabled until
you configure real credentials, a funded wallet, and explicit caps.

## What's new for Crypto World's Fair

Built between 2026-10-10 and 2026-10-12 on top of the runtime below:

- EURC as a configured asset, quoted like a stablecoin.
- Exact-output quotes: `POST /v1/rfq` with `output_amount` returns the input
  cost, rounded so the maker never under-collects.
- Third-party recipients: wallet settlement accepts `recipient`; the maker leg
  pays that wallet and the sender's redeem transaction creates the recipient's
  token account when it is missing.
- A Pay page at `/app`; the swap demo moved to `/app/swap`.

Everything before 2026-09-14 was built for the Solana Frontier Hackathon and is
prior work.

## What's in the repo

- **Rust crate** — the maker runtime and Axum HTTP API, with SQLite persistence.
- **`web/app`** — a Vite/React surface: the Pay page, the swap demo, and the read-only maker page.
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

Full setup — wallets, live mode, caps, EURC and cbBTC mints — is in
[docs/maker-setup.mdx](docs/maker-setup.mdx). The container reads its policy
from `FIRMAMENT_CONFIG_B64` (base64 of `config.toml`) when no file is mounted.

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
