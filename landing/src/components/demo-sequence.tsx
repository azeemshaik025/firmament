const runbookSteps = [
  ["01", "RFQ", "amount + pair"],
  ["02", "Gate", "quote or reject"],
  ["03", "Settle", "wallet HTLC"],
  ["04", "Record", "ledger movement"],
  ["05", "Repair", "rebalance / refill"],
];

export function DemoSequence() {
  return (
    <section className="demo-section" id="runbook">
      <div className="section-heading compact-heading">
        <p className="section-kicker">Runtime path</p>
        <h2>Quote. Gate. Settle. Record. Repair.</h2>
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
