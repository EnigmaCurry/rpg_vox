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
      '/room.ws': { target: BACKEND, ws: true },
      // `/join/<token>` is deliberately NOT here. The server has no
      // route for it either (see the router in src/web.rs): an invite
      // link is the app, and the token is read off `location.pathname`
      // and spent through `/api/door/join`, which is proxied. Proxied,
      // what came back was the *built* `index.html` out of the release
      // binary, asking for `/assets/index-<hash>.js` that this server
      // has never heard of -- so a fresh invite opened a blank page
      // with one 404 in the console, and `just room-ui` could not be
      // used with the only way a new person ever gets in. Left alone,
      // vite serves the app for it like any other address it does not
      // recognise, which is exactly what the server does.
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
