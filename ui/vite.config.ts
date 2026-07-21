import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tsconfigPaths from 'vite-tsconfig-paths';

const isDemoMode = process.env.VITE_ENV === 'demo';

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), tsconfigPaths()],
  server: {
    host: '0.0.0.0', // Accessible to other devices on the network
    port: 5173,
  },
  build: {
    // Emit sourcemaps for real builds; skip for the static MSW demo.
    sourcemap: !isDemoMode,
    // Default Vite output: `dist/` with hashed, content-addressed assets.
    // (Upstream free-sleep wrote a fixed `index.js` into ../server/public/ and
    //  committed it to git — podd builds cleanly from source instead.)
    outDir: 'dist',
  },
});
