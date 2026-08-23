// Build stamp for this bundle, replaced at build time by vite's `define`
// (see vite.config.ts, which derives it from PODD_VERSION or `git describe`).
// Replaces upstream free-sleep's committed `versionInfo.json`, which was frozen
// at their 2.1.5 and said nothing about the build actually running (issue #109).
declare const __PODD_UI_VERSION__: string;

export const UI_VERSION: string = __PODD_UI_VERSION__;
