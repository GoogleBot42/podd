# Work sources

Single owner of "where candidate work comes from" for this repo. `fetch-work`
and `unblock` both read this file rather than each re-deriving the list —
update it here, not in either skill.

1. **Gitea open issues.** `tea issues list --repo zuckerberg/podd --output simple --limit 100`
   (the default limit silently truncates at ~30 issues — always pass --limit)
   (see the `git-forges` skill for `tea` setup/auth). Read a specific issue
   with `tea issues <n> --repo zuckerberg/podd`. Authorship gotcha: `tea`
   shows every issue's author as "agent" because the migration re-created
   them. Issues ≤ #23 were migrated from GitHub and were filed by Jeremy
   (footer: "opened by GoogleBot42"); higher-numbered ones are agent-filed
   audit findings. "Tasks I/Jeremy filed" means the migrated set. The GitHub
   originals 404 (repo shadowbanned) — the Gitea copies are canonical.

2. **README.md "Status" paragraph's "Remaining work" list.** Read the block
   under `> Status:` near the top of `README.md` — it names the current
   remaining-work items in prose. As of 2026-08-30 that's: the hardware
   verification pass for the OS A/B OTA cycle, the sleep-detection half of
   biometrics (issue #141; vitals landed in PR #140), and an open Pod-4
   sensor MCU reliability item (auto-recovers; see
   `docs/research/pod4-sensor-protocol.md` §5). Don't
   trust this cached list — re-read the paragraph; it's the one place this is
   allowed to change.

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
   ~20 hits, concentrated in `pod-updater`/`podd-core` (`TODO(live-cutover)`
   markers gating live MCU/OS-update writes behind dry-run), plus a few
   one-off UI and OS-build TODOs (the pod-proto sensor decoding TODOs were
   closed by #42/#44).
