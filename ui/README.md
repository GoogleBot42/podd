# ui — podd web frontend

The `podd` web UI, vendored **source-only** from
[`throwaway31265/free-sleep`](https://github.com/throwaway31265/free-sleep)'s
`app/` (MIT). React 19 + TypeScript + Vite 7, MUI v7, `@tanstack/react-query`,
zustand, axios. See `LICENSE.md` for the upstream MIT license.

The SPA talks to the backend **same-origin** at `/api` in production
(`src/api/api.ts`): `podd` serves this bundle statically and answers `/api/*` on
the same port, with an `index.html` history-fallback for client-side routes.

## Building

```sh
# Local dev build (no external services required):
npm install        # regenerates package-lock.json if needed
npm run build      # → dist/  (index.html + hashed assets under dist/assets/)

# Backend-free demo (MSW mocks every /api/* route):
npm run build:demo # → dist/ , serves mock service worker

# Reproducible build via Nix (npm ci offline from the pinned lockfile):
nix build .#ui     # → result/ (== dist/)
```

`npm run build` runs `tsc -b` then `vite build`. Output goes to `dist/`, which is
git-ignored — build artifacts are never committed.

If `package-lock.json` changes, the Nix `npmDepsHash` in the repo-root
`flake.nix` must be regenerated: set it to `lib.fakeHash`, run `nix build .#ui`,
and copy the `got:` hash back.

## What changed vs upstream free-sleep

This is a clean-vendored fork, not a mirror. Deltas:

1. **Source only, no committed build output.** Upstream commits the built SPA
   (`server/public/index.js` ~2.2 MB + 8.4 MB sourcemap) and the compiled
   backend (`server/dist/`) into git. None of that is vendored; `dist/` and
   `node_modules/` are git-ignored and produced from source.
2. **`build.outDir` reset to the default `dist/`.** Upstream wrote the bundle
   straight into `../server/public/`. `vite.config.ts` now emits standard,
   content-hashed assets under `dist/assets/`.
3. **Cross-directory dependency on `server/` removed.** Upstream re-exports the
   shared zod schemas from `../../../server/src/...` and imports
   `server/src/serverInfo.json`. Those schema files are inlined under
   `src/api/schemas/` (see below) and the API shim files now import from there.
   `rg "\.\./.*server" src` returns nothing.
4. **Local version file.** `server/src/serverInfo.json` was replaced with a local
   `src/versionInfo.json` (`{version, branch}`); the "update available" check
   still compares against the upstream `serverInfo.json` on GitHub.
5. **Sentry plugin gated.** The `@sentry/vite-plugin` sourcemap upload (which
   needs `.env.sentry-build-plugin` + network) is opt-in behind
   `VITE_ENABLE_SENTRY_PLUGIN=true`; a default build has zero external coupling.
6. **`typescript` declared as a direct devDependency** (upstream relied on it
   being a hoisted transitive dep) so `tsc` resolves in a clean `npm ci` tree.
7. **Nix `buildNpmPackage`** added as `packages.<system>.ui` in `flake.nix` for a
   reproducible, offline build from the pinned `package-lock.json`.

### Inlined schemas (`src/api/schemas/`)

Copied verbatim from upstream `server/src/...` (internal imports rewritten to the
flat local dir); these are the canonical wire-type contract `podd`'s backend
reimplements:

`deviceStatusSchema.ts`, `schedulesSchema.ts`, `settingsSchema.ts`,
`serverStatusSchema.ts`, `servicesSchema.ts`, `jobsSchema.ts`, `timeZones.ts`,
`sleepRecordsSchema.ts`, `vitalsRecordSchema.ts`, `movementRecordSchema.ts`.

`README_APP.md` is the upstream app README, kept for reference.
