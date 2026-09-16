import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const BACKEND = process.env.RPG_VOX_BACKEND || 'http://127.0.0.1:7331';

// Every backend prefix exposed by crates/rpg_vox/src/http.rs. Vite serves
// the SPA at :5173 and forwards these paths through to the Rust backend on
// :7331. Anything the SPA fetches that isn't listed here gets Vite's own
// SPA fallback (index.html) instead — which then fails `r.json()` with an
// "unexpected character '<'" error. When you add a new route on the Rust
// side, add its top-level prefix here.
const BACKEND_ROUTES = [
  '/agents',
  '/chat',
  '/clicks',
  '/healthz',
  '/images',
  '/mixer',
  '/perf',
  '/playback',
  '/pw',
  '/record',
  '/samples',
  '/say',
  '/scenes',
  '/script',
  '/scripts',
  '/settings',
  '/state',
  '/voices',
  '/web-mic',
  '/widgets',
  '/workflow',
  '/workflows',
];

const proxy = Object.fromEntries(
  BACKEND_ROUTES.map((p) => [p, { target: BACKEND, changeOrigin: false }]),
);
// WebSocket path is proxied separately with `ws: true` so vite forwards the
// upgrade handshake and binary frames to the Rust backend.
proxy['/monitor.ws'] = { target: BACKEND, changeOrigin: false, ws: true };
proxy['/mic.ws']     = { target: BACKEND, changeOrigin: false, ws: true };

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
    // AudioWorklet.addModule() won't accept data URLs, so anything
    // named `*.worklet.js` must be emitted as an external asset even
    // when it's under the default 4 KiB inline threshold. Other assets
    // keep the default behavior.
    assetsInlineLimit: (filePath) => filePath.endsWith('.worklet.js') ? false : undefined,
  },
});
