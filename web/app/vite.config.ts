import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  base: '/app/',
  server: {
    port: 5174,
    strictPort: true,
    proxy: {
      '/v1': 'http://127.0.0.1:5050'
    }
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true
  }
});
