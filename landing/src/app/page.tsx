import { DemoSequence } from "@/components/demo-sequence";

const repoHref = "https://github.com/azeemshaik025/firmament";
const appHref = process.env.NEXT_PUBLIC_APP_URL ?? "/app";
const makerHref = `${appHref}/runtime`;
const docsHref = "/docs";

const proofPoints = [
  ["Quote", "Exact recipient amount"],
  ["Lock", "Rate held for 30 seconds"],
  ["Settle", "Atomic Solana HTLC"],
  ["Refill", "USDC through Circle Gateway"],
];

const runtimeBlocks = [
  {
    label: "Quote",
    title: "Exact-output RFQ",
    copy: "Ask for 89.30 EURC, get the USDC cost.",
    tone: "lime",
  },
  {
    label: "Policy",
    title: "Risk gate",
    copy: "Caps, inventory, stale prices.",
    tone: "amber",
  },
  {
    label: "Inventory",
    title: "EURC on hand",
    copy: "The maker holds the thin side.",
    tone: "cyan",
  },
  {
    label: "Settlement",
    title: "Solana HTLC",
    copy: "Both legs or neither.",
    tone: "blue",
  },
  {
    label: "Repair",
    title: "Rebalance + refill",
    copy: "Jupiter and Circle Gateway.",
    tone: "violet",
  },
  {
    label: "Ledger",
    title: "Double-entry proof",
    copy: "Every payment booked.",
    tone: "red",
  },
];

const productLanes = [
  {
    label: "Payment apps",
    title: "Promise a euro amount",
    copy: "Show the recipient's exact amount before the sender signs, and deliver exactly that.",
  },
  {
    label: "Payroll",
    title: "Pay contractors in their currency",
    copy: "USDC in, EURC out, one transaction, no slippage to explain.",
  },
  {
    label: "Makers",
    title: "Run the FX desk",
    copy: "Hold EURC inventory, quote under policy, refill USDC from Gateway.",
  },
];

const positionReasons = [
  "USDC has $465M of Solana DEX liquidity; EURC has $0.7M (Jupiter, Oct 2026)",
  "An AMM route cannot promise a recipient amount; a maker with inventory can",
  "Firm quote, then atomic settlement: the recipient gets the number or the sender is refunded",
  "Self-hosted maker runtime: any app can run its own",
];

const runtimeSignals = [
  ["Health", "runtime status"],
  ["Inventory", "USDC / EURC / SOL"],
  ["Flow", "quotes, locks, payouts"],
  ["Repair", "Jupiter + Gateway"],
  ["Ledger", "balances + P&L"],
];

const apiProof = [
  ["Quote", "POST /v1/rfq { output_amount }"],
  ["Pay", "wallet-settlement { recipient }"],
  ["Runtime", "GET state + events"],
  ["Ledger", "balances + trades"],
];

const footerSignals = [
  { code: "01", label: "Exact recipient amounts" },
  { code: "02", label: "Firm quotes under policy" },
  { code: "03", label: "Atomic HTLC settlement" },
  { code: "04", label: "Gateway-refilled inventory" },
];

export default function Home() {
  return (
    <main id="top">
      <header className="site-header" aria-label="Firmament navigation">
        <a className="brand" href="#top" aria-label="Firmament home">
          <span className="brand-mark" aria-hidden="true">
            <span />
            <span />
            <span />
            <span />
          </span>
          <span>Firmament</span>
        </a>
        <nav aria-label="Primary">
          <a href="#product">Product</a>
          <a href="#runtime">Maker</a>
          <a href="#proof">Proof</a>
          <a href="#api">API</a>
          <a href={docsHref}>Docs</a>
        </nav>
        <a className="nav-action" href={appHref}>
          <span>Try a payment</span>
          <span className="button-arrow" aria-hidden="true">
            {"→"}
          </span>
        </a>
      </header>
      <nav className="mobile-nav" aria-label="Mobile sections">
        <a href="#product">Product</a>
        <a href="#runtime">Maker</a>
        <a href="#api">API</a>
        <a href={docsHref}>Docs</a>
      </nav>

      <section className="hero" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="eyebrow">Firm FX for stablecoin payments</p>
          <h1 id="hero-title">Firmament</h1>
          <p className="hero-lede">
            The rate you promise is the rate they get. Firmament quotes an exact
            recipient amount in EURC, holds the USDC cost for 30 seconds, and
            settles both legs atomically on Solana.
          </p>
          <div className="hero-actions">
            <a className="button button-primary" href={appHref}>
              <span>Try a payment</span>
              <span className="button-arrow" aria-hidden="true">
                {"→"}
              </span>
            </a>
            <a
              className="button button-secondary"
              href={repoHref}
              target="_blank"
              rel="noreferrer"
            >
              View on GitHub
            </a>
          </div>
        </div>

        <div className="hero-board" aria-label="Firmament runtime block map">
          <div className="board-top">
            <span>exact-output quotes</span>
            <span>USDC in, EURC out</span>
          </div>
          <div className="block-stack" aria-hidden="true">
            <span className="piece piece-lime wide">EURC inventory</span>
            <span className="piece piece-amber">Policy</span>
            <span className="piece piece-cyan">Jupiter</span>
            <span className="piece piece-blue wide">HTLC</span>
            <span className="piece piece-red">Ledger</span>
            <span className="piece piece-violet wide">Rebalance</span>
            <span className="piece piece-lime">Gateway refill</span>
            <span className="piece piece-cyan wide">Runtime API</span>
          </div>
          <div className="board-footer">
            <span>quote when safe</span>
            <span>deliver the exact amount</span>
          </div>
        </div>
      </section>

      <section className="proof-strip" aria-label="Project proof points">
        {proofPoints.map(([label, value]) => (
          <div key={label}>
            <span>{label}</span>
            <strong>{value}</strong>
          </div>
        ))}
      </section>

      <section className="section product-section" id="product">
        <div className="section-heading">
          <p className="section-kicker">Product position</p>
          <h2>Payments need a number, not a route.</h2>
          <p>
            Swap routers optimise what you pay. Payment apps have to promise
            what the other side receives. Firmament is the maker that makes that
            promise safe to show.
          </p>
        </div>
        <div className="lane-grid" aria-label="Firmament users">
          {productLanes.map((lane) => (
            <article className="lane-card" key={lane.label}>
              <span>{lane.label}</span>
              <h3>{lane.title}</h3>
              <p>{lane.copy}</p>
            </article>
          ))}
        </div>
        <div className="position-panel" aria-label="Why Firmament is different">
          <div>
            <span>Why now</span>
            <h3>
              Stablecoin payments are going multi-currency, and the non-USD side
              on Solana is thin.
            </h3>
          </div>
          <ul>
            {positionReasons.map((reason) => (
              <li key={reason}>{reason}</li>
            ))}
          </ul>
        </div>
      </section>

      <section className="section runtime-section" id="runtime">
        <div className="section-heading">
          <p className="section-kicker">Live maker</p>
          <h2>See the desk the maker is running.</h2>
          <p>
            The public maker page shows USDC and EURC inventory, HTLC
            settlements, rebalances, Gateway refills, and ledger-backed
            accounting.
          </p>
        </div>
        <div className="runtime-layout">
          <div className="terminal-window" aria-label="Live maker preview">
            <div className="terminal-title">
              <span>firmament::maker</span>
              <span>read-only projection</span>
            </div>
            <div className="terminal-grid">
              {runtimeSignals.map(([label, value]) => (
                <div className="terminal-row" key={label}>
                  <span>{label}</span>
                  <strong>{value}</strong>
                </div>
              ))}
            </div>
          </div>
          <div className="runtime-cta-panel">
            <div>
              <span className="runtime-panel-label">Maker console</span>
              <h3>Inspect inventory, flow, and the repair loop.</h3>
              <p>
                Open the read-only maker view to see whether the book is healthy
                after quotes and payouts.
              </p>
            </div>
            <div className="runtime-checks" aria-label="Maker console properties">
              <span>Public</span>
              <span>Read-only</span>
              <span>Ledger-backed</span>
            </div>
            <a className="button button-primary runtime-button" href={makerHref}>
              Open the maker page
            </a>
          </div>
        </div>
      </section>

      <section className="section proof-section" id="proof">
        <div className="section-heading">
          <p className="section-kicker">Runtime proof</p>
          <h2>A payment, end to end.</h2>
        </div>
        <div className="proof-matrix">
          {runtimeBlocks.map((block) => (
            <article className={`proof-card ${block.tone}`} key={block.label}>
              <span className="proof-label">{block.label}</span>
              <h3>{block.title}</h3>
              <p>{block.copy}</p>
            </article>
          ))}
        </div>
      </section>

      <DemoSequence />

      <section className="section api-section" id="api">
        <div className="api-panel">
          <div className="api-copy">
            <p className="section-kicker">HTTP API</p>
            <h2>A small API for firm FX.</h2>
            <p>
              Quote an exact output, start wallet settlement with a recipient,
              and read the same runtime state the maker uses.
            </p>
            <a className="docs-callout" href={docsHref}>
              <span>Developer docs</span>
              <strong>API routes, runtime model, and maker setup.</strong>
              <em>
                Open docs
                <span className="button-arrow" aria-hidden="true">
                  {"→"}
                </span>
              </em>
            </a>
          </div>
          <div className="api-list" aria-label="API proof surface">
            {apiProof.map(([label, value]) => (
              <div className="api-item" key={label}>
                <span>{label}</span>
                <code>{value}</code>
              </div>
            ))}
          </div>
        </div>
      </section>

      <footer className="site-footer" aria-label="Firmament footer">
        <div className="footer-wordmark" aria-hidden="true">
          <span>Firm</span>
          <span>ament</span>
        </div>
        <div className="footer-content">
          <div className="footer-brand">
            <span className="brand-mark footer-mark" aria-hidden="true">
              <span />
              <span />
              <span />
              <span />
            </span>
            <div>
              <strong>
                <span>Firm</span>ament
              </strong>
              <p>Firm FX for stablecoin payments on Solana.</p>
            </div>
          </div>
          <div className="footer-actions" aria-label="Footer links">
            <a href={repoHref} target="_blank" rel="noreferrer">
              <span>GitHub</span>
              <span className="button-arrow" aria-hidden="true">
                {"→"}
              </span>
            </a>
            <a href={makerHref}>Maker</a>
            <a href={docsHref}>Docs</a>
          </div>
        </div>
        <div className="footer-signals" aria-label="Runtime capabilities">
          {footerSignals.map((signal) => (
            <span key={signal.code}>
              <b>{signal.code}</b>
              {signal.label}
            </span>
          ))}
        </div>
      </footer>
    </main>
  );
}
