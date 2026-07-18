import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tsconfigPaths from 'vite-tsconfig-paths';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const isDemoMode = process.env.VITE_ENV === 'demo';

// Sentry sourcemap upload is opt-in. It requires an auth token
// (.env.sentry-build-plugin) and network access, so a default `npm run build`
// has zero external coupling. Enable with VITE_ENABLE_SENTRY_PLUGIN=true.
const enableSentryPlugin = process.env.VITE_ENABLE_SENTRY_PLUGIN === 'true';

// https://vitejs.dev/config/
export default defineConfig(async () => {
  const plugins = [react(), tsconfigPaths()];

  if (enableSentryPlugin) {
    const { sentryVitePlugin } = await import('@sentry/vite-plugin');
    const versionInfoPath = fileURLToPath(
      new URL('./src/versionInfo.json', import.meta.url),
    );
    const info = JSON.parse(readFileSync(versionInfoPath, 'utf8'));
    plugins.push(
      sentryVitePlugin({
        org: process.env.SENTRY_ORG || 'free-sleep',
        project: process.env.SENTRY_PROJECT || 'app',
        release: {
          name: info.version,
        },
      }),
    );
  }

  return {
    plugins,
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
  };
});
