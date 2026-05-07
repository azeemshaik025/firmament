import type { Metadata } from "next";
import type { ReactNode } from "react";
import "./globals.css";

export const metadata: Metadata = {
  title: "Firmament | Solana RFQ Maker Runtime",
  description:
    "Firmament is managed liquidity infrastructure for Solana apps and treasuries: firm RFQs, risk-aware settlement, book repair, and ledger-backed runtime proof.",
  openGraph: {
    title: "Firmament | Solana RFQ Maker Runtime",
    description:
      "Managed inventory, firm RFQs, Solana HTLC settlement, book repair, and read-only runtime proof.",
    type: "website",
  },
  twitter: {
    card: "summary_large_image",
    title: "Firmament | Solana RFQ Maker Runtime",
    description:
      "Managed inventory, firm RFQs, Solana HTLC settlement, book repair, and runtime proof.",
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
