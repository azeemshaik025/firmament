import { DemoSequence } from "@/components/demo-sequence";

const repoHref = "https://github.com/azeemshaik025/firmament";
const runtimeHref = "https://github.com/azeemshaik025/firmament/blob/v0.1.0/docs/runtime-proof.mdx";
const docsHref = "/docs";

const proofPoints = [
  ["Reserve", "One USDC Gateway source"],
  ["Source", "Jupiter routes configured assets"],
  ["RFQ", "Firm quote or refusal"],
  ["Repair", "Rebalance the book"],
];

const runtimeBlocks = [
  {
    label: "Reserve",
    title: "Gateway USDC",
    copy: "One source of liquidity.",
    tone: "lime",
  },
  {
    label: "Source",
    title: "Jupiter routes",
    copy: "Source configured assets.",
    tone: "cyan",
  },
  {
    label: "Policy",
    title: "Clean quote gate",
    copy: "Quote only when safe.",
    tone: "amber",
  },
  {
    label: "Settlement",
    title: "Solana HTLC",
    copy: "Lock, redeem, refund.",
    tone: "blue",
  },
  {
    label: "Repair",
    title: "Book repair loop",
    copy: "Rebalance and refill.",
    tone: "violet",
  },
  {
    label: "Ledger",
    title: "Accounting proof",
    copy: "Balances, trades, P&L.",
    tone: "red",
  },
];

const productLanes = [
  {
    label: "Apps",
    title: "Offer controlled swaps",
    copy: "Serve firm quotes from a maker-run book without routing users away.",
  },
  {
    label: "Treasuries",
    title: "Maintain one reserve",
    copy: "Keep USDC in Gateway while Firmament handles sourcing and repair.",
  },
  {
    label: "Builders",
    title: "Integrate the runtime",
    copy: "Request RFQs, settle wallet flow, and inspect live maker state.",
  },
];

const positionReasons = [
  "Maintain one USDC source in Circle Gateway",
  "Use Jupiter when a quote needs a configured destination asset",
  "Reject unsafe flow before settlement",
  "Rebalance after fills to repair the book",
];

const runtimeSignals = [
  ["Health", "runtime status"],
  ["Reserve", "Gateway USDC"],
  ["Flow", "working / escrow"],
  ["Repair", "Jupiter + Gateway"],
  ["Ledger", "balances + P&L"],
];

const apiProof = [
  ["RFQ", "POST /v1/rfq"],
  ["Settle", "wallet HTLC flow"],
  ["Runtime", "GET state + events"],
  ["Ledger", "balances + trades"],
];

const footerSignals = [
  { code: "01", label: "Single USDC reserve" },
  { code: "02", label: "Jupiter-sourced assets" },
  { code: "03", label: "Policy-gated RFQs" },
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
          <a href={docsHref}>Docs</a>
        </nav>
        <a
          className="nav-action"
          href={repoHref}
          target="_blank"
          rel="noreferrer"
        >
          <span>GitHub</span>
          <span className="button-arrow" aria-hidden="true">
            {"\u2192"}
          </span>
        </a>
      </header>
      <nav className="mobile-nav" aria-label="Mobile sections">
        <a href="#product">Product</a>
        <a href="#runtime">Runtime</a>
        <a href="#api">API</a>
        <a href={docsHref}>Docs</a>
      </nav>

      <section className="hero" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="eyebrow">Solana RFQ Maker Runtime</p>
          <h1 id="hero-title">Firmament</h1>
          <p className="hero-lede">
            Keep one USDC reserve. Source configured assets through Jupiter.
            Gate RFQs with policy, settle on Solana HTLCs, and repair inventory
            after each fill.
          </p>
          <div className="hero-actions">
            <a
              className="button button-primary"
              href={repoHref}
              target="_blank"
              rel="noreferrer"
            >
              <span>View on GitHub</span>
              <span className="button-arrow" aria-hidden="true">
                {"\u2192"}
              </span>
            </a>
            <a className="button button-secondary" href="#runtime">
              Inspect runtime
            </a>
          </div>
        </div>

        <div className="hero-board" aria-label="Firmament runtime block map">
          <div className="board-top">
            <span>single liquidity reserve</span>
            <span>Gateway USDC to Jupiter routes</span>
          </div>
          <div className="block-stack" aria-hidden="true">
            <span className="piece piece-lime wide">Gateway USDC</span>
            <span className="piece piece-amber">Policy</span>
            <span className="piece piece-cyan">Jupiter</span>
            <span className="piece piece-blue wide">HTLC</span>
            <span className="piece piece-red">Ledger</span>
            <span className="piece piece-violet wide">Rebalance</span>
            <span className="piece piece-lime">Refill</span>
            <span className="piece piece-cyan wide">Runtime API</span>
          </div>
          <div className="board-footer">
            <span>quote when safe</span>
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
          <h2>One reserve. Firm quotes. A repaired book.</h2>
          <p>
            Firmament keeps USDC as the maker&apos;s source of liquidity, uses
            Jupiter when a destination asset is needed, and repairs inventory
            after fills.
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
            <span>Why it matters</span>
            <h3>
              Swap routers handle one trade. Firmament keeps the maker&apos;s book
              alive.
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
          <p className="section-kicker">Live Runtime</p>
          <h2>See the book the maker is operating.</h2>
          <p>
            The public runtime shows Gateway reserve, working inventory, HTLC
            settlement, rebalances, refills, and ledger-backed accounting.
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
              <h3>Inspect the reserve, flow, and repair loop.</h3>
              <p>
                Open the read-only app view to see whether the liquidity book is
                healthy after quotes and fills.
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
              Inspect runtime
            </a>
          </div>
        </div>
      </section>

      <section className="section proof-section" id="proof">
        <div className="section-heading">
          <p className="section-kicker">Runtime proof</p>
          <h2>The operating loop, not just a swap.</h2>
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
            <h2>Small API for managed liquidity.</h2>
            <p>
              Request a firm quote, start wallet settlement, and inspect the
              same runtime state the maker uses.
            </p>
            <a className="docs-callout" href={docsHref}>
              <span>Developer docs</span>
              <strong>API routes, runtime model, and demo setup.</strong>
              <em>
                Open docs
                <span className="button-arrow" aria-hidden="true">
                  {"\u2192"}
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
              <p>Execution infrastructure for Solana apps and treasuries.</p>
            </div>
          </div>
          <div className="footer-actions" aria-label="Footer links">
            <a href={repoHref} target="_blank" rel="noreferrer">
              <span>GitHub</span>
              <span className="button-arrow" aria-hidden="true">
                {"\u2192"}
              </span>
            </a>
            <a href={runtimeHref} target="_blank" rel="noreferrer">
              Runtime
            </a>
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
