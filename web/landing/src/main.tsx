import React from 'react';
import ReactDOM from 'react-dom/client';
import './styles.css';

const metrics = [
  ['Inventory-first', 'fills before rebalances'],
  ['Solana-only', 'USDC / SOL / cbBTC'],
  ['Tiny mainnet', '$2 default caps'],
  ['Operator web', 'plus HTTP control plane']
];

const rails = [
  'Firm quotes with risk-aware spread and inventory skew',
  'Live HTLC settlement using maker and taker wallets',
  'Jupiter rebalance checks after fills',
  'Circle Gateway USDC refill supervision',
  'Append-only ledger with P&L projection'
];

function App() {
  return (
    <main className="site-shell">
      <section className="hero-grid" aria-labelledby="hero-title">
        <div className="orb orb-a" />
        <div className="orb orb-b" />
        <div className="hero-copy">
          <p className="eyebrow">Solana liquidity operations, not a swap toy</p>
          <h1 id="hero-title">Firmament turns treasury inventory into firm RFQ liquidity.</h1>
          <p className="lede">
            A maker runtime for apps and treasuries that quote from managed USDC, SOL, and cbBTC inventory,
            reject unsafe flow, settle through live Solana HTLCs, and keep the book healthy with automated refill
            and rebalance loops.
          </p>
          <div className="cta-row" aria-label="Primary actions">
            <a className="button button-primary" href="/app">Launch swap console</a>
            <a className="button button-secondary" href="/app/admin">Open admin cockpit</a>
          </div>
        </div>

        <aside className="terminal-card" aria-label="Runtime status preview">
          <div className="terminal-topline">
            <span>firmament://maker-runtime</span>
            <span className="pulse">live</span>
          </div>
          <div className="route-line">
            <span>USDC</span>
            <b />
            <span>SOL</span>
            <b />
            <span>cbBTC</span>
          </div>
          <div className="console-lines">
            <p><span>risk</span> allowlist ok, notional below cap</p>
            <p><span>quote</span> spread 38 bps, expires in 42s</p>
            <p><span>settle</span> htlc terms ready for taker lock</p>
            <p><span>ledger</span> escrow, fee, spread entries balanced</p>
          </div>
        </aside>
      </section>

      <section className="metric-strip" aria-label="Runtime highlights">
        {metrics.map(([value, label]) => (
          <article key={value}>
            <strong>{value}</strong>
            <span>{label}</span>
          </article>
        ))}
      </section>

      <section className="ops-board" aria-labelledby="ops-title">
        <div>
          <p className="eyebrow">Demo proof</p>
          <h2 id="ops-title">A cockpit for controlled mainnet liquidity.</h2>
        </div>
        <div className="rail-list">
          {rails.map((rail, index) => (
            <div className="rail-item" key={rail}>
              <span>{String(index + 1).padStart(2, '0')}</span>
              <p>{rail}</p>
            </div>
          ))}
        </div>
      </section>
    </main>
  );
}

ReactDOM.createRoot(document.getElementById('root')!).render(<App />);
