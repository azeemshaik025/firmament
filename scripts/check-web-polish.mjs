import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

const root = new URL("..", import.meta.url).pathname;

const read = (path) => {
  const absolutePath = join(root, path);
  return existsSync(absolutePath) ? readFileSync(absolutePath, "utf8") : "";
};

const readBuffer = (path) => {
  const absolutePath = join(root, path);
  return existsSync(absolutePath) ? readFileSync(absolutePath) : Buffer.alloc(0);
};

const landingFavicon = readBuffer("landing/src/app/favicon.ico");
const docsJson = JSON.parse(read("docs/docs.json") || "{}");
const docsOpenApi = read("docs/openapi.yaml");

const checks = [
  {
    label: "Landing CSS should not import remote fonts at paint time",
    ok: !read("landing/src/app/globals.css").includes("@import url("),
    detail: "Use next/font in landing/src/app/layout.tsx so display fonts are preloaded by Next.",
  },
  {
    label: "App CSS should not import remote fonts at paint time",
    ok: !read("web/app/src/styles.css").includes("@import url("),
    detail: "Load the app font stylesheet from web/app/index.html before the Vite bundle.",
  },
  {
    label: "Landing should use Next font variables",
    ok:
      read("landing/src/app/layout.tsx").includes("next/font/google") &&
      read("landing/src/app/layout.tsx").includes("--font-bangers") &&
      read("landing/src/app/globals.css").includes('--font-bangers: "Bangers"') &&
      read("landing/src/app/globals.css").includes("--font-display: var(--font-bangers)"),
    detail: "The landing layout should own font preloading and CSS variable wiring.",
  },
  {
    label: "Landing production start should match standalone output",
    ok:
      read("landing/package.json").includes(".next/standalone/.next/static") &&
      read("landing/package.json").includes("cd .next/standalone && node server.js"),
    detail: "Stage static assets next to the standalone server before starting local production preview.",
  },
  {
    label: "Landing should declare custom favicon assets",
    ok:
      landingFavicon.length > 6 &&
      landingFavicon.readUInt16LE(0) === 0 &&
      landingFavicon.readUInt16LE(2) === 1 &&
      landingFavicon.readUInt16LE(4) === 3 &&
      read("landing/src/app/icon.svg").includes("Firmament favicon") &&
      read("landing/src/app/layout.tsx").includes("icons:"),
    detail: "Replace the scaffold favicon with the Firmament mark and expose it through Next metadata.",
  },
  {
    label: "App should self-host and preload critical fonts",
    ok:
      !read("web/app/index.html").includes("fonts.googleapis.com") &&
      read("web/app/index.html").includes("/fonts/bangers-latin.woff2") &&
      read("web/app/src/styles.css").includes("@font-face") &&
      read("landing/vercel.json").includes("/fonts/:path*"),
    detail: "Use local font files and route them from the landing domain so the app does not depend on Google Fonts at first paint.",
  },
  {
    label: "App runtime config script should not trigger Vite HTML warnings",
    ok:
      !read("web/app/index.html").includes("runtime-env.js") &&
      read("web/app/src/main.tsx").includes("runtime-env.js") &&
      read("web/app/src/main.tsx").includes("@vite-ignore"),
    detail: "Load runtime-env.js from app code with an ignored dynamic import so Vite does not treat it as an HTML bundle entry.",
  },
  {
    label: "Runtime route should not eagerly load wallet code",
    ok:
      read("web/app/src/main.tsx").includes("lazy(") &&
      !read("web/app/src/main.tsx").includes("from './SolanaWalletProvider'") &&
      read("web/app/src/pages/SwapRoute.tsx").includes("SolanaWalletProvider"),
    detail: "Keep the wallet provider inside the lazy swap route so /app/runtime stays lightweight.",
  },
  {
    label: "Docs should include production surface checks",
    ok: read("docs/demo-evidence-checklist.md").includes("Production surface polish"),
    detail: "The demo checklist should remind submitters to verify landing, app, and docs polish.",
  },
  {
    label: "Docs API reference should expose the interactive try-out playground",
    ok:
      docsJson.api?.playground?.display === "interactive" &&
      docsJson.api?.playground?.proxy === true &&
      docsJson.api?.examples?.prefill === true,
    detail: "Enable Mintlify's interactive playground, proxy-backed deployed API calls, and example prefill.",
  },
  {
    label: "Docs OpenAPI spec should default to deployed API and retain local backend",
    ok:
      docsOpenApi.includes("servers:") &&
      docsOpenApi.includes("url: https://api.firmament.shaikazeem.com") &&
      docsOpenApi.indexOf("url: https://api.firmament.shaikazeem.com") <
        docsOpenApi.indexOf("url: http://127.0.0.1:5050") &&
      docsOpenApi.includes("url: http://127.0.0.1:5050"),
    detail: "OpenAPI-generated try-out pages should default to production while keeping local demos selectable.",
  },
];

const failures = checks.filter((check) => !check.ok);

if (failures.length > 0) {
  console.error("Web polish check failed:");
  for (const failure of failures) {
    console.error(`- ${failure.label}: ${failure.detail}`);
  }
  process.exit(1);
}

console.log(`Web polish check passed (${checks.length} checks).`);
