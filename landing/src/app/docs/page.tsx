import Link from "next/link";

const docs = [
  {
    label: "Start",
    title: "Quickstart",
    copy: "Run the API, app, docs preview, and the first runtime reads.",
    href: "https://github.com/azeemshaik025/firmament/blob/v0.1.0/docs/quickstart.mdx"
  },
  {
    label: "Design",
    title: "Architecture",
    copy: "How RFQs, policy, settlement, repair, and public runtime proof fit together.",
    href: "https://github.com/azeemshaik025/firmament/blob/v0.1.0/docs/architecture.mdx"
  },
  {
    label: "Flow",
    title: "Swap Flow",
    copy: "The app-facing quote, wallet settlement, lock, and redeem path.",
    href: "https://github.com/azeemshaik025/firmament/blob/v0.1.0/docs/swap-flow.mdx"
  },
  {
    label: "Proof",
    title: "Runtime Proof",
    copy: "Public runtime state, ledger balances, trades, and network verification.",
    href: "https://github.com/azeemshaik025/firmament/blob/v0.1.0/docs/runtime-proof.mdx"
  },
  {
    label: "Ops",
    title: "Maker Setup",
    copy: "Mainnet config, safety caps, Gateway, Jupiter, and funded maker inventory.",
    href: "https://github.com/azeemshaik025/firmament/blob/v0.1.0/docs/maker-setup.mdx"
  },
  {
    label: "API",
    title: "Backend Reference",
    copy: "OpenAPI routes for RFQs, wallet settlement, runtime state, ledger, and trades.",
    href: "https://github.com/azeemshaik025/firmament/blob/v0.1.0/docs/openapi.yaml"
  }
];

export default function DocsPage() {
  return (
    <main className="docs-page">
      <header className="docs-header">
        <Link className="brand" href="/" aria-label="Firmament home">
          <span className="brand-mark" aria-hidden="true">
            <span />
            <span />
            <span />
            <span />
          </span>
          <span>Firmament</span>
        </Link>
        <a className="nav-action" href="/app" target="_blank" rel="noreferrer">
          <span>Launch app</span>
          <span className="button-arrow" aria-hidden="true">
            {"\u2192"}
          </span>
        </a>
      </header>

      <section className="docs-hero">
        <p className="eyebrow">Developer docs</p>
        <h1>Build against the runtime, not a black box.</h1>
        <p>
          Firmament keeps the implementation notes, setup guide, API contract,
          and runtime proof path close to the demo so builders can verify what
          is actually running.
        </p>
      </section>

      <section className="docs-grid" aria-label="Firmament documentation">
        {docs.map((doc) => (
          <a className="docs-card" href={doc.href} target="_blank" rel="noreferrer" key={doc.title}>
            <span>{doc.label}</span>
            <h2>{doc.title}</h2>
            <p>{doc.copy}</p>
          </a>
        ))}
      </section>
    </main>
  );
}
