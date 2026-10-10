const steps = [
  {
    title: "Quote",
    copy: "Ask for the recipient's exact EURC amount. The maker answers with the USDC cost from its own inventory, under its policy caps.",
  },
  {
    title: "Lock",
    copy: "The sender's USDC goes into a Solana HTLC. The rate is held for 30 seconds.",
  },
  {
    title: "Match",
    copy: "The maker locks the exact EURC for the recipient against the same secret hash.",
  },
  {
    title: "Deliver",
    copy: "The sender reveals the secret to release the recipient's EURC; the maker collects the USDC with the same secret. An unmatched lock refunds after expiry.",
  },
];

export function DemoSequence() {
  return (
    <section className="section" id="payment">
      <div className="section-heading">
        <p className="section-kicker">How it settles</p>
        <h2>One secret, two escrows, no slippage.</h2>
      </div>
      <ol className="sequence">
        {steps.map((step) => (
          <li key={step.title}>
            <h3>{step.title}</h3>
            <p>{step.copy}</p>
          </li>
        ))}
      </ol>
    </section>
  );
}
