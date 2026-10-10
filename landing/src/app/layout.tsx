import type { Metadata } from "next";
import { IBM_Plex_Sans, JetBrains_Mono } from "next/font/google";
import localFont from "next/font/local";
import type { ReactNode } from "react";
import "./globals.css";

const cabinet = localFont({
  display: "swap",
  variable: "--font-cabinet",
  src: [
    { path: "../../public/fonts/cabinet-grotesk-500.woff2", weight: "500", style: "normal" },
    { path: "../../public/fonts/cabinet-grotesk-700.woff2", weight: "700", style: "normal" },
  ],
});

const ibmPlexSans = IBM_Plex_Sans({
  display: "swap",
  subsets: ["latin"],
  variable: "--font-plex",
  weight: ["400", "500"],
});

const jetBrainsMono = JetBrains_Mono({
  display: "swap",
  subsets: ["latin"],
  variable: "--font-jetbrains",
  weight: ["400"],
});

const description =
  "Firm FX for stablecoin payments on Solana: quote an exact EURC amount, hold the USDC cost for 30 seconds, settle both legs atomically.";

export const metadata: Metadata = {
  title: "Firmament, firm FX for stablecoin payments",
  description,
  icons: {
    icon: [
      { url: "/favicon.svg", type: "image/svg+xml" },
      { url: "/favicon.ico", sizes: "16x16 32x32 48x48" },
      { url: "/icon.svg", type: "image/svg+xml" },
    ],
    shortcut: "/favicon.ico",
  },
  openGraph: {
    title: "Firmament, firm FX for stablecoin payments",
    description,
    type: "website",
  },
  twitter: {
    card: "summary",
    title: "Firmament, firm FX for stablecoin payments",
    description,
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: ReactNode;
}>) {
  return (
    <html lang="en">
      <body className={`${cabinet.variable} ${ibmPlexSans.variable} ${jetBrainsMono.variable}`}>
        {children}
      </body>
    </html>
  );
}
