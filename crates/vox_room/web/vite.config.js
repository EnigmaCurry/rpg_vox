import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const BACKEND = process.env.VOX_ROOM_BACKEND || 'http://127.0.0.1:7340';

// `pnpm dev` serves the app on :5173 and forwards the backend's paths
// (see crates/vox_room/src/web.rs) to the running vox_room.
export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': { target: BACKEND },
      '/join': { target: BACKEND },
      '/room.ws': { target: BACKEND, ws: true },
    },
  },
  build: {
    outDir: '../dist-ui',
    emptyOutDir: true,
    sourcemap: false,
    // AudioWorklet.addModule() won't take data URLs, so worklets are
    // always emitted as files.
    assetsInlineLimit: (filePath) => (filePath.endsWith('.worklet.js') ? false : undefined),
  },
});
