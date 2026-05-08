import type { Metadata } from "next";
import { Bangers, IBM_Plex_Sans, JetBrains_Mono } from "next/font/google";
import type { ReactNode } from "react";
import "./globals.css";

const bangers = Bangers({
  display: "block",
  subsets: ["latin"],
  variable: "--font-bangers",
  weight: "400",
});

const ibmPlexSans = IBM_Plex_Sans({
  display: "swap",
  subsets: ["latin"],
  variable: "--font-plex",
  weight: ["400", "500", "600", "700"],
});

const jetBrainsMono = JetBrains_Mono({
  display: "swap",
  subsets: ["latin"],
  variable: "--font-jetbrains",
  weight: ["400", "500", "600", "700", "800"],
});

export const metadata: Metadata = {
  title: "Firmament | Solana RFQ Maker Runtime",
  description:
    "Firmament is managed liquidity infrastructure for Solana apps and treasuries: firm RFQs, risk-aware settlement, book repair, and ledger-backed runtime proof.",
  icons: {
    icon: [
      { url: "/favicon.svg", type: "image/svg+xml" },
      { url: "/favicon.ico", sizes: "16x16 32x32 48x48" },
      { url: "/icon.svg", type: "image/svg+xml" },
    ],
    shortcut: "/favicon.ico",
  },
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
      <body className={`${bangers.variable} ${ibmPlexSans.variable} ${jetBrainsMono.variable}`}>
        {children}
      </body>
    </html>
  );
}
