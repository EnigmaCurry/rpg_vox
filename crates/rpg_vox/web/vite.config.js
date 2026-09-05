import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const BACKEND = process.env.RPG_VOX_BACKEND || 'http://127.0.0.1:7331';

const proxy = Object.fromEntries(
  ['/say', '/chat', '/settings', '/workflows', '/workflow', '/pw', '/healthz']
    .map((p) => [p, { target: BACKEND, changeOrigin: false }]),
);
// WebSocket path is proxied separately with `ws: true` so vite forwards the
// upgrade handshake and binary frames to the Rust backend.
proxy['/monitor.ws'] = { target: BACKEND, changeOrigin: false, ws: true };

export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 5173,
    strictPort: true,
    proxy,
  },
  build: {
    outDir: '../dist-ui',
    emptyOutDir: true,
    sourcemap: false,
  },
});
