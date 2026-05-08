const runbookSteps = [
  ["01", "Reserve", "Gateway USDC"],
  ["02", "Source", "Jupiter route"],
  ["03", "Gate", "quote or refuse"],
  ["04", "Settle", "Solana HTLC"],
  ["05", "Repair", "rebalance / refill"],
];

export function DemoSequence() {
  return (
    <section className="demo-section" id="runbook">
      <div className="section-heading compact-heading">
        <p className="section-kicker">Runtime path</p>
        <h2>Reserve. Source. Gate. Settle. Repair.</h2>
      </div>
      <div className="flow-rail" aria-label="Firmament runtime path">
        {runbookSteps.map(([index, label, detail]) => (
          <article className="flow-step" key={label}>
            <span>{index}</span>
            <strong>{label}</strong>
            <p>{detail}</p>
          </article>
        ))}
      </div>
    </section>
  );
}
