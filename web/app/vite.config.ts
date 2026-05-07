import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

const devPort = Number(process.env.FIRMAMENT_WEB_PORT ?? '3000');
const apiProxyTarget = process.env.FIRMAMENT_API_PROXY_TARGET ?? 'http://127.0.0.1:5050';

export default defineConfig({
  plugins: [react()],
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
      }
    }
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true
  }
});
