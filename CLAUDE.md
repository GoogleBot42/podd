# podd — agent notes

FOSS replacement firmware/OS for the Eight Sleep Pod. **This code controls a
real bed with sleeping people in it** (heating and vibration alarms). Treat
anything on the actuation path as safety-critical.

## Safety tripwires (each one has burned us)

- MCU writes are gated by `PODD_DRY_RUN` (defaults to dry-run). Never deploy
  alarm-capable changes to a live Pod without dry-run verification first.
- The sensor MCU is a zombie for ~60 s after any podd restart: it streams
  telemetry and answers Ping but **silently ignores alarm/actuation writes**.
  Actuation-critical writes must retry until the firmware confirms — never
  fire-and-forget. (Deliberate duplication with
  `.claude/rules/actuation-safety.md` — keep both copies.)
- LSP framing has **no byte-stuffing**: a `0x7E` byte anywhere in a payload gets
  the whole frame silently dropped by the MCUs. Setpoints and alarm frames go
  through delimiter-safe nudging, not escaping — new variable-payload commands
  need the same (see `.claude/rules/actuation-safety.md`).
- Alarms must not arm before NTP sync — there is no RTC battery.
- Config migrations and generated configs must never inject default alarm
  blocks. That exact bug fired a real alarm on a real bed (2026-07-20).
- eMMC (`mmcblk2`) is **never** a write target; everything boots from SD so the
  stock card can be swapped back. After any raw media write, verify it:
  `cmp -n <byte-count> image.img /dev/sdX`.
- `printf '%s'` does not expand `\t`/`\n` escapes — a literal `\t` in a
  generated fstab once took out the `/data` mount. Use `printf '%b'`, `echo`,
  or literal characters when generating config lines in shell.
- Under `set -e`, a `[ -n "$X" ] && cmd` in an EXIT trap returns 1 when `$X`
  is empty and fails the whole script *after* it succeeded (cost two 2-hour
  CI OS builds, 2026-09-02). Use `if`/`then` in traps and cleanup functions.

## Build & test

- `cargo test` / `cargo build` — Rust workspace under `crates/`. The Nix dev
  shell (`nix develop`) provides the toolchain.
- The tree carries rustfmt drift and CI does not enforce fmt — a bare
  `cargo fmt` (even `-p <crate>`) sweeps a dozen unrelated files into your
  diff. Format manually or revert the unrelated files before committing
  (cost a force-push cycle 2026-08-30).
- Cross builds: `nix build .#podd-aarch64` (static aarch64-musl), also
  `.#podup`, `.#ui`. Flake builds see only **git-tracked** files — a brand-new
  `.rs` file is invisible until `git add`ed, failing with E0583 "file not
  found for module" even though `cargo` builds fine.
- Don't run `scripts/build-release.sh` inside `nix develop`: the dev shell
  sets `SOURCE_DATE_EPOCH`, which makes podup's mksquashfs die with
  "SOURCE_DATE_EPOCH and command line options can't be used at the same
  time". Run it bare (it only needs `nix` on PATH).
- After any `ui/package-lock.json` change, `nix build .#ui` fails with a hash
  mismatch — paste the reported "got:" hash into `npmDepsHash` in `flake.nix`.
- OS image: `os/scripts/build.sh`. Buildroot only works inside the
  `.#buildrootEnv` FHS sandbox (plain nix shells fail its `dependencies.sh`);
  the script handles this — see `os/README.md`. Read the `build-sd-image`
  skill before touching this area.
- UI dev: `npm run dev|build|lint` in `ui/` (React 19, Vite, MUI). A fresh
  worktree has no `node_modules` — run `npm ci` first or lint dies with
  "eslint: command not found".

## Git & forge

- Canonical forge is Gitea: `git.neet.dev/zuckerberg/podd` (`origin`). The
  GitHub mirror (`github` remote) never takes issues or PRs, but it is the
  **public release channel**: its Actions build+publish the release when a
  `v*` tag arrives via the push mirror (docs/RELEASING.md owns the flow).
  Use the `git-forges` skill for tea/issue/PR mechanics.
- Commit work as you go: small commits per logical unit, never batched at the
  end. `main` is protected (server-enforced) — all work goes up as a branch
  plus a PR; reference the issue number when there is one.
- Merge your own PRs once tests pass and the work is done (Jeremy authorized
  this 2026-08-15) — don't leave finished PRs waiting on him. Jeremy reviews
  post-merge.
- After merging a change that affects the running Pod (daemon, UI, config),
  deploy it to the live Pod via the `deploy-live-pod` skill — merged-but-
  undeployed work is unfinished. Live-Pod cautions in that skill and
  `CLAUDE.local.md` still apply.
- Every commit ends with the `Co-Authored-By` and `Claude-Session` trailers.

## Docs map (one owner per fact — point, don't restate)

- `docs/ARCHITECTURE.md` as-built design; `docs/REPLACEMENT_PLAN.md` original plan.
- `docs/CLEANROOM-OS.md` from-source OS image + bring-up field notes;
  `docs/SD-BOOT.md` SD boot-flow reference (the legacy L1 stock-clone
  image + scripts were deleted 2026-08-31; git history has them).
- `docs/FLASHING.md`, `INSTALL.md`, `RECOVERY.md`, `UPDATING.md`,
  `RELEASING.md` — user/maintainer guides (releases: tag `v*`, CI does the rest).
- `docs/ALARM-TESTING.md` — staged live-alarm verification protocol (kill
  switches, dismissal tests, day-attribution gotcha).
- `docs/research/` — dense reverse-engineering evidence; reference material,
  don't read it all to orient.

## Working with Jeremy

- Clean-room strictness: NXP/Variscite vendor blobs are acceptable; Eight Sleep
  binaries must never ship or be silently depended on. If a shortcut would
  violate that, flag it and ask — don't take it.
- Routine deploys of merged work (binary/UI swap + podd restart) are
  pre-authorized (2026-08-15) — see the deploy rule under "Git & forge".
  Still confirm before anything beyond that: flashing, power-cycling, config
  pushes that change alarm/actuation semantics. Live-device access details
  live in `CLAUDE.local.md` (untracked).
- When handing Jeremy a built artifact (SD image, binary), point him at the
  ready-to-use uncompressed file — he should never have to gunzip/extract
  anything himself. Keep the raw `.img` in `dist/` alongside the `.gz`.
- Use subagents for large parallelizable subtasks. Run them on Opus or Sonnet
  (`model: "opus"` / `model: "sonnet"`) wherever those suffice — searches,
  routine edits, mechanical sweeps; reserve the top-tier model for work that
  actually needs it. Keep designs simple unless asked to extend. Agents
  editing the same worktree in parallel must be told "never `git stash`" and
  given disjoint paths — a stash/pop by one agent grabs the other's in-flight
  edits (near-miss 2026-08-23).
- When Jeremy dumps logs or bootlogs unprompted, that is a request to
  re-diagnose, not to acknowledge.

Skills: `deploy-live-pod` (dev-loop binary/config deploys), `build-sd-image`
(OS image + won't-boot post-mortem), `fetch-work` / `unblock` / `reflect` (meta).
After substantial work, run the `reflect` skill before wrapping up ("no changes
needed" is its normal outcome — don't skip it because nothing seems stale).
