# Work sources

Single owner of "where candidate work comes from" for this repo. `fetch-work`
and `unblock` both read this file rather than each re-deriving the list —
update it here, not in either skill.

1. **Gitea open issues.** `tea issues list --repo zuckerberg/podd --output simple`
   (see the `git-forges` skill for `tea` setup/auth). Read a specific issue
   with `tea issues <n> --repo zuckerberg/podd`.

2. **README.md "Status" paragraph's "Remaining work" list.** Read the block
   under `> Status:` near the top of `README.md` — it names the current
   remaining-work items in prose. As of 2026-08-15 that's: RAUC A/B OTA
   wiring, decoding the Pod-4 sensor's biometric packet payloads, and an open
   Pod-4 sensor MCU reliability item (auto-recovers; see
   `docs/research/pod4-sensor-protocol.md` §5). Don't trust this cached list —
   re-read the paragraph; it's the one place this is allowed to change.

3. **`docs/REPLACEMENT_PLAN.md` planned-vs-done gaps.** The plan (esp. §6
   "Staged roadmap" and §9 "Update architecture") describes phases/features
   that may be ahead of what's actually landed. Compare its claims against
   `docs/ARCHITECTURE.md` (as-built) and the codebase to find gaps — this
   source requires judgment, not a single grep.

4. **Project memory dir.** `~/.claude/projects/-home-googlebot-workspace-eightsleep-podd/memory/`
   — read `MEMORY.md` there for transient statuses and pending decisions not
   yet written down anywhere durable (CLAUDE.md, docs, issues).

5. **TODO/FIXME comments in the tree.**
   `grep -rn "TODO\|FIXME" crates/ ui/src/ os/ install/ scripts/` — currently
   ~26 hits, concentrated in `pod-updater`/`podd-core` (`TODO(live-cutover)`
   markers gating live MCU/OS-update writes behind dry-run), `pod-proto`
   sensor packet/command decoding gaps, plus a few one-off UI and OS-build
   TODOs.
