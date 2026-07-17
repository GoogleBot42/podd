# ui — podd web frontend

Vendored from free-sleep's `app/` (React 19 + TypeScript + Vite 7, MUI, react-query),
MIT-licensed. **Source only** — free-sleep committed 10+ MB of built JS into git
(`server/public/index.js`, `server/dist/`); we do not. The SPA is built with Nix
(`buildNpmPackage`) and served as static assets by `podd`'s `api` crate with SPA
history-fallback.

To vendor (planned): copy free-sleep `app/` source, inline the shared zod schemas
from `server/src/...` into `app/src/api/schemas/`, set Vite `build.outDir` back to
the default `dist/`, and gitignore build output. Preserve upstream MIT LICENSE.md
and the in-app license modal.
