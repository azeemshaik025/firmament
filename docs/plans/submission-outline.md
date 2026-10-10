# Submission outline, Crypto World's Fair

Facts and structure for the arena project page. Write the prose yourself;
judges read the fields as the team's words.

## Deadline

Submissions close 2026-10-12 23:59 PDT, which is 2026-10-13 12:29 IST.

## Fields

Name: Firmament

Tagline (one line): Firm FX for stablecoin payments on Solana. The rate you
promise is the rate they get.

Category: Payments & Remittance.

Links:

- Landing: https://firmament-landing.vercel.app
- Repo: https://github.com/azeemshaik025/firmament (branch `payments-fx`,
  merge to `main` before submitting so the links resolve)
- Demo video: `video/out/firmament-demo.mp4` uploaded to YouTube (unlisted is fine)
- Pitch video: your Loom, 2 to 3 minutes, you on camera
- Runtime proof doc: `docs/runtime-proof.mdx` in the repo

## Description, in order

1. The problem in one sentence: a payment app has to promise what the
   recipient receives, and on Solana the non-USD side is thin (USDC ~$465M of
   DEX liquidity vs EURC ~$0.7M, Jupiter, Oct 2026), so a route cannot make
   that promise.
2. What Firmament does: a self-hosted maker that quotes an exact EURC amount,
   holds the USDC cost for 30 seconds, and settles both legs through paired
   Solana HTLC escrows under one secret, to the sender or to any wallet address.
3. The proof: two mainnet payments of 1 EURC on 2026-10-10 (one to a brand-new
   wallet whose token account was created inside the redeem). Signatures are in
   the runtime proof doc and the demo video.
4. How it works, four steps: quote from the maker's own inventory under policy
   caps; sender locks USDC; maker locks the exact EURC for the recipient under
   the same hash; sender reveals the secret, both legs clear or both refund.
5. What is underneath: Rust runtime, policy-gated RFQs, Jupiter Swap API v2
   rebalancing (a real rebalance ran after the first payment), Circle Gateway
   USDC refill path, double-entry SQLite ledger, public maker page, two-call
   HTTP API.
6. Business: spread on conversions; customers are payment, payroll, and
   remittance apps that need a euro amount they can show before signing; makers
   can run their own desk on the open-source runtime.
7. What is next: more currencies as Circle issues them, a hosted maker, an MCP
   tool so agents can pay in a recipient's currency.

## Prior work disclosure

Required by the rules. State it plainly in the description:

- The maker runtime, HTLC settlement, Jupiter and Gateway integrations, ledger,
  and the swap/runtime pages were built for the Solana Frontier Hackathon
  (before 2026-09-14) and are prior work.
- Built during this hackathon: EURC support, exact-output quotes, third-party
  recipients on settlement, the Pay page, the redesign, the landing, the demo
  signer, and the fixes found during the live run (maker token-account creation
  inside the redeem, restart-safe leg restore).

## Team

Solo: Azeem Shaik, GitHub and X `azeemshaik025`.

## Builder updates

Post two or three short updates on the arena before submitting. Material:

- Oct 10: redesign landed; first mainnet payment settled; the maker's missing
  USDC token account was found in the live run and fixed in the redeem path.
- Oct 11: demo video and runtime proof published.
- Oct 12: submission.

## Pitch video script (2 to 3 minutes, you on camera)

0:00 Who you are and one sentence: Firmament is firm FX for stablecoin
payments on Solana.

0:15 The problem. Show the two liquidity numbers. A payment app cannot promise
a euro amount through a route; it can only estimate.

0:45 What Firmament does. Exact-output quote, 30-second hold, atomic
settlement, any recipient. Cut to the Pay page clip for ten seconds.

1:20 Why it is safe. Two HTLCs, one secret. The maker never takes USDC without
the EURC clearing; the sender refunds if the maker never locks.

1:45 Proof. Two mainnet payments today, signatures on the maker page and in the
repo. Mention it ran from a laptop with tiny amounts; no hosted app yet.

2:10 Business and ask. Spread on conversions, payment and payroll apps as
customers, the runtime is open source so a maker can run its own desk. What
you want from Colosseum.

2:40 Close with the tagline.
