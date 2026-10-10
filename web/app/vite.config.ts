import { readFileSync } from 'node:fs';
import { defineConfig, Plugin } from 'vite';
import react from '@vitejs/plugin-react';

const devPort = Number(process.env.FIRMAMENT_WEB_PORT ?? '3000');
const apiProxyTarget = process.env.FIRMAMENT_API_PROXY_TARGET ?? 'http://127.0.0.1:5050';
const demoSignerPath = process.env.FIRMAMENT_DEMO_SIGNER_PATH;
const rpcProxyTarget = process.env.FIRMAMENT_RPC_PROXY_TARGET ?? 'https://api.mainnet-beta.solana.com';

// Dev only: serves the keypair at FIRMAMENT_DEMO_SIGNER_PATH so the "Demo wallet" can sign
// without a browser extension. Never set the variable outside a local recording.
const demoSigner: Plugin = {
  name: 'firmament-demo-signer',
  apply: 'serve',
  configureServer(server) {
    server.middlewares.use('/__demo-signer', (request, response) => {
      // This runs ahead of Vite's host check, so refuse anything but the loopback host
      // ourselves; a DNS-rebinding page must not be able to read the keypair.
      const loopback = /^(127\.0\.0\.1|localhost)(:\d+)?$/.test(request.headers.host ?? '');
      if (!demoSignerPath || !loopback) {
        response.statusCode = 404;
        response.end();
        return;
      }
      response.setHeader('content-type', 'application/json');
      response.end(readFileSync(demoSignerPath));
    });
  }
};

export default defineConfig({
  plugins: [react(), demoSigner],
  base: '/app/',
  define: {
    global: 'globalThis'
  },
  resolve: {
    alias: {
      buffer: 'buffer/'
    }
  },
  optimizeDeps: {
    include: ['buffer']
  },
  server: {
    host: '127.0.0.1',
    port: devPort,
    strictPort: true,
    proxy: {
      '/health': {
        target: apiProxyTarget,
        changeOrigin: true
      },
      '/v1': {
        target: apiProxyTarget,
        changeOrigin: true
      },
      // The public RPCs answer 403 to browser-originated token-account reads, so the dev server
      // forwards /rpc without the browser headers. Point VITE_SOLANA_RPC_URL at it for local runs.
      '/rpc': {
        target: rpcProxyTarget,
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/rpc/, ''),
        configure: (proxy) => {
          proxy.on('proxyReq', (proxyRequest) => {
            proxyRequest.removeHeader('origin');
            proxyRequest.removeHeader('referer');
          });
        }
      }
    }
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true
  }
});
