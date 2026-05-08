import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  allowedDevOrigins: ["127.0.0.1"],
  devIndicators: false,
  output: process.env.VERCEL === "1" ? "export" : "standalone",
  turbopack: {
    root: process.cwd(),
  },
};

export default nextConfig;
