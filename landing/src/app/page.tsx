import { DemoSequence } from "@/components/demo-sequence";

const appHref = "http://127.0.0.1:3000/app/";
const runtimeHref = `${appHref}runtime`;

const proofPoints = [
  ["Assets", "USDC / SOL / cbBTC"],
  ["RFQ", "Firm quotes or clean refusals"],
  ["Settle", "Solana HTLC path"],
  ["Ledger", "One accounting source"],
];

const runtimeBlocks = [
  {
    label: "Inventory",
    title: "Ledger balances",
    copy: "Custody, Gateway, escrow.",
    tone: "lime",
  },
  {
    label: "RFQ",
    title: "Firm terms",
    copy: "Price, spread, inventory.",
    tone: "cyan",
  },
  {
    label: "Risk",
    title: "Quote gate",
    copy: "Unsafe flow is refused.",
    tone: "amber",
  },
  {
    label: "Settlement",
    title: "HTLC status",
    copy: "Lock, redeem, refund.",
    tone: "blue",
  },
  {
    label: "Repair",
    title: "Book repair",
    copy: "Rebalance and refill.",
    tone: "violet",
  },
  {
    label: "Ledger",
    title: "Runtime proof",
    copy: "Events, trades, accounting.",
    tone: "red",
  },
];

const productLanes = [
  {
    label: "Apps",
    copy: "Embed governed liquidity.",
  },
  {
    label: "Treasuries",
    copy: "Control inventory flow.",
  },
  {
    label: "Builders",
    copy: "Use a small HTTP API.",
  },
];

const runtimeSignals = [
  ["Health", "runtime status"],
  ["Inventory", "custody / gateway / escrow"],
  ["Trades", "total + successful"],
  ["Repair", "rebalance + refill"],
  ["Ledger", "source of truth"],
];

const apiProof = [
  ["RFQ", "POST /v1/rfq"],
  ["Settle", "wallet settlement"],
  ["Trades", "GET /v1/trades/{id}"],
  ["Runtime", "state + events"],
];

const footerSignals = [
  { code: "01", label: "Inventory-aware RFQs" },
  { code: "02", label: "Policy-gated settlement" },
  { code: "03", label: "Ledger-backed accounting" },
  { code: "04", label: "Automated book repair" },
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
          <a href="#runtime">Runtime</a>
          <a href="#proof">Proof</a>
          <a href="#api">API</a>
        </nav>
        <a
          className="nav-action"
          href={appHref}
          target="_blank"
          rel="noreferrer"
        >
          Launch app
        </a>
      </header>

      <section className="hero" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="eyebrow">Solana RFQ Maker Runtime</p>
          <h1 id="hero-title">Firmament</h1>
          <p className="hero-lede">
            Managed liquidity for Solana apps and treasuries, with firm RFQs,
            policy checks, HTLC settlement, and ledger-backed book repair.
          </p>
          <div className="hero-actions">
            <a
              className="button button-primary"
              href={appHref}
              target="_blank"
              rel="noreferrer"
            >
              Launch app
            </a>
            <a className="button button-secondary" href="#runtime">
              View runtime
            </a>
          </div>
        </div>

        <div className="hero-board" aria-label="Firmament runtime block map">
          <div className="board-top">
            <span>managed liquidity book</span>
            <span>USDC / SOL / cbBTC</span>
          </div>
          <div className="block-stack" aria-hidden="true">
            <span className="piece piece-lime wide">Inventory</span>
            <span className="piece piece-amber">Policy</span>
            <span className="piece piece-cyan">RFQ</span>
            <span className="piece piece-blue wide">HTLC</span>
            <span className="piece piece-red">Ledger</span>
            <span className="piece piece-violet wide">Book repair</span>
            <span className="piece piece-lime">Gateway</span>
            <span className="piece piece-cyan wide">Runtime API</span>
          </div>
          <div className="board-footer">
            <span>quote from inventory</span>
            <span>settle, record, repair</span>
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
          <h2>Controlled liquidity, not another swap widget.</h2>
          <p>
            Quote from managed inventory. Reject unsafe flow. Settle and repair
            the book.
          </p>
        </div>
        <div className="lane-grid" aria-label="Firmament users">
          {productLanes.map((lane) => (
            <article className="lane-card" key={lane.label}>
              <span>{lane.label}</span>
              <p>{lane.copy}</p>
            </article>
          ))}
        </div>
      </section>

      <section className="section runtime-section" id="runtime">
        <div className="section-heading">
          <p className="section-kicker">Live Runtime</p>
          <h2>Runtime state, open for inspection.</h2>
          <p>
            Health, balances, trades, repair state, and ledger-backed
            accounting are available from the public app surface.
          </p>
        </div>
        <div className="runtime-layout">
          <div className="terminal-window" aria-label="Live Runtime preview">
            <div className="terminal-title">
              <span>firmament::runtime</span>
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
              <span className="runtime-panel-label">Runtime console</span>
              <h3>Inspect the live book.</h3>
              <p>
                Open the read-only app view for health, inventory, trades, and
                ledger-backed balances.
              </p>
            </div>
            <div className="runtime-checks" aria-label="Runtime console properties">
              <span>Public</span>
              <span>Read-only</span>
              <span>Ledger-backed</span>
            </div>
            <a
              className="button button-primary runtime-button"
              href={runtimeHref}
              target="_blank"
              rel="noreferrer"
            >
              Open runtime
            </a>
          </div>
        </div>
      </section>

      <section className="section proof-section" id="proof">
        <div className="section-heading">
          <p className="section-kicker">Runtime proof matrix</p>
          <h2>The pieces that matter.</h2>
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
          <div>
            <p className="section-kicker">HTTP API</p>
            <h2>Small API. Real runtime state.</h2>
            <p>
              Request quotes, settle accepted flow, and inspect runtime state.
            </p>
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
              <p>Execution infrastructure for Solana apps and treasuries.</p>
            </div>
          </div>
          <div className="footer-actions" aria-label="Footer links">
            <a href={appHref} target="_blank" rel="noreferrer">
              Launch app
            </a>
            <a href={runtimeHref} target="_blank" rel="noreferrer">
              Runtime
            </a>
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
