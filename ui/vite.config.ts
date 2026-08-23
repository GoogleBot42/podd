import { execFileSync } from 'node:child_process';
import { rmSync } from 'node:fs';
import { resolve } from 'node:path';
import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import tsconfigPaths from 'vite-tsconfig-paths';

const isDemoMode = process.env.VITE_ENV === 'demo';

// Build stamp for the bundle (issue #109), matching what crates/podd-core's
// build.rs stamps into the daemon: `PODD_VERSION` from the environment (Nix
// passes it — `nix build .#ui` gets no `.git`), else `git describe`, else
// 'dev'. Never a made-up version. Displayed by the Settings page so a bundle
// that was deployed out of step with the daemon is visible instead of silent.
const uiVersion = ((): string => {
  const fromEnv = process.env.PODD_VERSION?.trim();
  if (fromEnv) return fromEnv.replace(/^v/, '');
  try {
    return execFileSync('git', ['describe', '--tags', '--always', '--dirty=-dirty'], {
      cwd: __dirname,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim().replace(/^v/, '') || 'dev';
  } catch {
    return 'dev';
  }
})();

// The MSW worker script in public/ is only registered by the static demo
// build; keep it out of every other bundle so it never ships to the Pod.
const stripMswWorker = (): Plugin => ({
  name: 'strip-msw-worker',
  apply: 'build',
  closeBundle() {
    if (!isDemoMode) {
      rmSync(resolve(__dirname, 'dist/mockServiceWorker.js'), { force: true });
    }
  },
});

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), tsconfigPaths(), stripMswWorker()],
  define: {
    __PODD_UI_VERSION__: JSON.stringify(uiVersion),
  },
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
