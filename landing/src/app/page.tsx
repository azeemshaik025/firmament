import { DemoSequence } from "@/components/demo-sequence";

const repoHref = "https://github.com/azeemshaik025/firmament";
const demoHref = process.env.NEXT_PUBLIC_DEMO_URL ?? "#payment";
const makerHref = `${repoHref}/blob/main/docs/runtime-proof.mdx`;
const docsHref = "/docs";

const lanes = [
  {
    label: "Payment apps",
    title: "Promise a euro amount",
    copy: "Show the recipient's exact amount before the sender signs, and deliver exactly that.",
  },
  {
    label: "Payroll",
    title: "Pay contractors in their currency",
    copy: "USDC in, EURC out, one settlement, no slippage to explain.",
  },
  {
    label: "Makers",
    title: "Run the FX desk",
    copy: "Hold EURC inventory, quote under policy, refill USDC from Circle Gateway.",
  },
];

const spec = [
  ["Assets", "USDC, EURC, SOL"],
  ["Quote", "Exact output amount, cost held for 30 seconds"],
  ["Recipient", "Any Solana wallet address; the token account is created if missing"],
  ["Settlement", "Two Solana HTLCs under one secret: the recipient's leg releases first, the maker collects with the same secret, an unmatched lock refunds after expiry"],
  ["Inventory", "The maker's own wallet, rebalanced through Jupiter Swap API v2, USDC refilled through Circle Gateway"],
  ["Accounting", "Double-entry SQLite ledger with per-trade P&L"],
  ["Interface", "HTTP API: POST /v1/rfq with output_amount, then wallet-settlement with recipient"],
  ["Code", "Rust runtime and React app, MIT or Apache-2.0"],
];

const makerShows = [
  ["Inventory", "USDC, EURC, SOL on hand"],
  ["Flow", "quotes, locks, deliveries, refunds"],
  ["Repair", "Jupiter rebalances, Gateway refills"],
  ["Ledger", "balances per account, P&L per trade"],
  ["Proof", "every Solana signature, linked"],
];

const faq = [
  {
    q: "Is this running on mainnet?",
    a: "Yes, with tiny amounts. The demo maker holds a few euros of EURC and runs from a laptop. There is no hosted app yet; the repo runs the whole stack locally.",
  },
  {
    q: "Who holds the money during a payment?",
    a: "The HTLC programs on Solana. The sender's USDC and the maker's EURC each sit in escrow until the secret is revealed, or the locks expire and refund.",
  },
  {
    q: "What if the maker never locks its side?",
    a: "The sender refunds after expiry. The maker can only collect the USDC with the secret the sender reveals to collect the EURC, so the maker is never paid before the recipient.",
  },
  {
    q: "What is new for the Crypto World's Fair?",
    a: "EURC, exact-output quotes, third-party recipients, and the Pay page. The maker runtime underneath was built for the Solana Frontier hackathon and is disclosed as prior work.",
  },
];

export default function Home() {
  return (
    <main id="top">
      <header className="site-header">
        <a className="brand" href="#top" aria-label="Firmament home">
          <span className="brand-mark" aria-hidden="true" />
          <span>Firmament</span>
        </a>
        <nav className="nav-links" aria-label="Primary">
          <a href="#payment">How it settles</a>
          <a href="#maker">Maker</a>
          <a href="#spec">Spec</a>
          <a href={docsHref}>Docs</a>
        </nav>
        <a className="nav-action" href={repoHref} target="_blank" rel="noreferrer">
          <span>GitHub</span>
          <span className="button-arrow" aria-hidden="true">
            {"→"}
          </span>
        </a>
      </header>

      <section className="hero" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="page-kicker">Firm FX for stablecoin payments on Solana</p>
          <h1 id="hero-title">The rate you promise is the rate they get.</h1>
          <p className="hero-lede">
            Firmament quotes an exact EURC amount for the recipient, holds the
            USDC cost for 30 seconds, and settles both legs through escrows
            that open with one secret. The number shown before signing is the
            number delivered.
          </p>
          <div className="hero-actions">
            <a className="button button-primary" href={demoHref}>
              <span>Watch a payment</span>
              <span className="button-arrow" aria-hidden="true">
                {"→"}
              </span>
            </a>
            <a className="button button-secondary" href={repoHref} target="_blank" rel="noreferrer">
              View on GitHub
            </a>
          </div>
        </div>

        <figure className="hero-figure">
          <div className="quote-card" aria-label="A quote on the Pay page">
            <div className="quote-card-head">
              <span>Send EURC</span>
            </div>
            <div className="quote-amount">
              <span>Recipient gets</span>
              <strong>
                89.30<small>EURC</small>
              </strong>
            </div>
            <div className="quote-row">
              <span>Recipient</span>
              <strong>5u15DUL…fQFMp</strong>
            </div>
            <div className="quote-row quote-row-cost">
              <span>You pay</span>
              <strong>100.46 USDC</strong>
            </div>
            <div className="quote-row">
              <span>Rate</span>
              <strong>1 USDC = 0.8889 EURC</strong>
            </div>
            <div className="quote-lock" style={{ "--lock": 0.8 }}>
              <span>Held</span>
              <div className="lock-track">
                <i className="lock-fill" />
                <i className="lock-point" />
              </div>
              <span>24s</span>
            </div>
            <div className="quote-button">Pay 100.46 USDC</div>
          </div>
          <figcaption>
            The Pay page mid-quote, with example amounts. The line is the hold:
            it drains for 30 seconds while the rate stays fixed.
          </figcaption>
        </figure>
      </section>

      <section className="stat-strip" aria-label="Why a maker, not a route">
        <div>
          <strong>$465M</strong>
          <span>USDC liquidity on Solana DEXs</span>
        </div>
        <div>
          <strong>$0.7M</strong>
          <span>EURC liquidity on Solana DEXs</span>
        </div>
        <div>
          <strong>30 s</strong>
          <span>the cost is held on every quote</span>
        </div>
        <p className="stat-source">Liquidity figures from Jupiter token data, October 2026. A route through that EURC depth cannot promise a recipient amount; a maker holding EURC can.</p>
      </section>

      <DemoSequence />

      <section className="section" id="who">
        <div className="section-heading">
          <p className="section-kicker">Who it is for</p>
          <h2>Payments need a number, not a route.</h2>
          <p>
            Swap routers optimise what you pay. Payment apps have to promise
            what the other side receives. Firmament is the maker that makes
            that promise safe to show.
          </p>
        </div>
        <div className="lanes">
          {lanes.map((lane) => (
            <article className="lane" key={lane.label}>
              <span>{lane.label}</span>
              <h3>{lane.title}</h3>
              <p>{lane.copy}</p>
            </article>
          ))}
        </div>
      </section>

      <section className="section" id="maker">
        <div className="split">
          <div>
            <div className="section-heading">
              <p className="section-kicker">The maker</p>
              <h2>The desk behind the quote.</h2>
              <p>
                A self-hosted runtime any app can run. It quotes from its own
                inventory under policy caps, settles through escrow, repairs
                the book, and books every payment in a ledger you can read.
              </p>
            </div>
            <a className="text-link" href={makerHref} target="_blank" rel="noreferrer">
              Read the runtime proof
            </a>
          </div>
          <ul className="plain-list" aria-label="What the maker page shows">
            {makerShows.map(([label, value]) => (
              <li key={label}>
                <span>{label}</span>
                <span>{value}</span>
              </li>
            ))}
          </ul>
        </div>
      </section>

      <section className="section" id="spec">
        <div className="split">
          <div className="section-heading">
            <p className="section-kicker">Spec</p>
            <h2>Small surface, firm guarantees.</h2>
            <p>
              Two HTTP calls to pay: quote an exact output, then start wallet
              settlement with a recipient. Everything else is the maker's job.
            </p>
          </div>
          <dl className="spec">
            {spec.map(([term, detail]) => (
              <div key={term}>
                <dt>{term}</dt>
                <dd>{detail}</dd>
              </div>
            ))}
          </dl>
        </div>
      </section>

      <section className="section" id="faq">
        <div className="section-heading">
          <p className="section-kicker">Straight answers</p>
          <h2>What this is, and what it is not yet.</h2>
        </div>
        <div className="faq">
          {faq.map((item) => (
            <div key={item.q}>
              <h3>{item.q}</h3>
              <p>{item.a}</p>
            </div>
          ))}
        </div>
      </section>

      <footer className="site-footer">
        <p className="footer-statement">
          The rate you promise <em>is the rate they get.</em>
        </p>
        <div className="footer-row">
          <span>Firmament. Firm FX for stablecoin payments on Solana.</span>
          <nav className="footer-links" aria-label="Footer">
            <a href={repoHref} target="_blank" rel="noreferrer">GitHub</a>
            <a href={makerHref} target="_blank" rel="noreferrer">Runtime proof</a>
            <a href={docsHref}>Docs</a>
          </nav>
        </div>
      </footer>
    </main>
  );
}
