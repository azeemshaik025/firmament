# Deploy notes

Live surfaces (Crypto World's Fair entry):

- Landing: https://firmament-landing.vercel.app (Vercel project `firmament-landing`, static export; links to the repo and the runtime-proof doc)
- Web app: https://firmament-app.vercel.app/app is deployed but unlinked; its `/v1/*` rewrite targets `firmament-api.fly.dev`, which does not exist. The demo runs the API and app locally.
- API: local only for the hackathon (`cargo run` with `.env` + `config.toml`, bind 127.0.0.1:5050). RPC is `https://api.mainnet-beta.solana.com`: PublicNode returns 403 for token-account methods and dRPC 400.

Optional Fly.io deploy (`fly.toml` is committed; the account needs a payment method):

```bash
flyctl apps create firmament-api --org personal
flyctl volumes create firmament_data --app firmament-api --region bom --size 1 --yes
flyctl secrets set --app firmament-api --stage \
  SOLANA_RPC_URL=https://solana-rpc.publicnode.com \
  CIRCLE_GATEWAY_SOLANA_ADDRESS=9yovryyWxBLcmhuR92jFYq3Sc9kdmJrfFdsZ56gy5Mud \
  FIRMAMENT_CONFIG_B64="$(base64 -i config.toml)"
printf 'MAKER_KEYPAIR_JSON=%s\n' "$(cat ~/.config/solana/firmament-maker.json)" | flyctl secrets import --app firmament-api --stage
flyctl deploy --app firmament-api --remote-only
curl -s https://firmament-api.fly.dev/health
```

Optional, raises Jupiter rate limits (run in fish so the key is typed, not pasted into history):

```fish
flyctl secrets set --app firmament-api JUPITER_API_KEY=(read -s -P "Jupiter key: ")
```

`config.toml` is the production policy (gitignored): EURC enabled, cbBTC disabled, caps $5 per action / $25 per day, bind 0.0.0.0, SQLite at `/data/runtime.sqlite`.
