---
name: deploy-live-pod
description: Use when iterating on podd — pushing a freshly-built binary or an edited config.ron to Jeremy's already-running Pod over SSH for a quick dev-loop check. This is NOT the release installer (install/podd-install.sh, install/podd-slot-install.sh) — those are for versioned releases onto a fresh/rooted unit. This skill is for "I changed some Rust or some config, get it onto the live bed and see what happens."
---

# Deploy to the live Pod (dev loop)

This is a **real bed with sleeping people in it**. Heating and vibration
actuation are gated behind `PODD_DRY_RUN` (default: dry-run) — see the
"Safety tripwires" section of the root `CLAUDE.md` and
`.claude/rules/actuation-safety.md`. Read those before touching
anything here.

**Device specifics (host, port, key path, current on-device layout) are not
in this file** — see `CLAUDE.local.md` (untracked) for Jeremy's live-device
access and layout. That file also says the on-device paths have **drifted
across sessions** — confirm the current layout before assuming any path below
still holds.

## Before anything else

- **Know which deploys are pre-authorized.** Routine deploys of *merged*
  work — binary/UI swap + podd restart — are pre-authorized (CLAUDE.md,
  2026-08-15; restated in CLAUDE.local.md) and don't need a confirmation
  round-trip. **Everything beyond that still needs Jeremy's confirmation
  first**: flashing, power-cycling, config pushes that change
  alarm/actuation semantics, deploying unmerged/experimental code. He'll do
  the "is that fine?" round-trip and can run physical tests (e.g. double-tap
  dismissal) when given a clear protocol. Exception inside the exception: a
  config change that merely *applies a setting Jeremy explicitly asked for*
  (e.g. his bug report says a toggle is off but the device ignores it) is
  implementing his stated intent — do it, back up first, and report it
  prominently.
- Check whether the live unit overrides `PODD_DRY_RUN` via a systemd drop-in
  (`/etc/systemd/system/podd.service.d/*.conf`) **before** assuming the
  binary's compiled-in default (dry-run=true) is what's actually running.
  Jeremy's unit has run with real (non-dry-run) writes armed via exactly such
  a drop-in in the past — check `systemctl cat podd` on-device, don't guess.
- Never touch eMMC (`mmcblk2`-class device, i.e. whatever the on-device
  layout note in `CLAUDE.local.md` says is eMMC). Everything here is a
  userland binary/config swap over SSH; nothing here should ever target a
  block device.

## Procedure — deploying a rebuilt binary

1. **Sanity-check first.** `cargo test -p podd-core` (or whichever crate you
   actually touched — `podd`, `api`, `pod-proto`, `pod-probe`, `pod-update`,
   `podup`; see `crates/*/Cargo.toml` for the exact package names). Don't
   skip this because the change "looks small" — this is the same daemon that
   drives heater setpoints.
2. **Cross-build.** `nix build .#podd-aarch64 -o result-podd-<label>` — give
   every iteration its own `-o` label (e.g. `result-podd-fix1`,
   `result-podd-fix2`) so you don't clobber a previous build you might still
   want to diff or roll back to. The binary lands at
   `result-podd-<label>/bin/podd` (static aarch64-musl).
   **`nix build .#...` builds the working tree, not a branch** — if the main
   checkout is dirty (e.g. Jeremy's WIP) or behind origin, you'll silently
   ship the wrong code (happened 2026-08-15: a just-merged feature was
   missing from the binary). Build from a clean worktree of origin/main
   (`git worktree add <tmp> origin/main`) and **verify before deploying**:
   `grep -a -c "<string only the new code contains>" result-.../bin/podd`.
   Also: parallel agent sessions have cross-deployed within minutes of each
   other (2026-08-16) — check `journalctl -u podd | grep "podd starting"`
   timestamps for deploys you didn't make before assuming yours is live.
3. **Copy it to the device as a staged file, not in place.** `scp` the binary
   to a `*.new`-style staging path on-device (see `CLAUDE.local.md` for the
   currently-live convention — it has been `/data/podd/podd.new` in recent
   sessions, installed with `install -m 0755` to `/usr/bin/podd`; the
   *release* layout is `/opt/podd/current/rootfs/podd`, which is a different
   thing — don't assume it's what's running). **Confirm the current binary
   path on-device before overwriting anything** — this has drifted before.
4. **Back up the running binary before replacing it.** Copy whatever is
   currently at the live binary path to a `.orig`-suffixed sibling
   (`podd.orig` is the established convention) so a bad deploy has an
   instant, obvious revert.
5. **Swap and restart:**
   ```
   systemctl stop podd
   install -m 0755 <staged .new path> <live binary path>
   systemctl start podd
   ```
   Then **wait at least ~12 seconds** before checking `systemctl is-active
   podd` — that's enough time for the process itself to come up and either
   stay up or crash-loop visibly. It is **not** enough time to judge whether
   actuation is healthy — see "The 60-second zombie window" below.
6. **Verify against the specific behavior you're chasing**, not just
   "is it running": `journalctl -u podd --since "2 minutes ago"` (or a tighter
   `--since` around when you restarted) and read for the actual symptom, not
   just the absence of a crash.

## Procedure — deploying UI only (no restart needed)

A UI-only change does **not** require stopping or restarting podd: the SPA is
served from disk per request (`PODD_SPA_DIR`, live value in `systemctl cat
podd` — `/usr/share/podd/ui` as of 2026-08-22), so an atomic dir swap is
picked up immediately and there is no zombie window and no bed disruption.
Since PR #97 clients revalidate `index.html`, so no stale-client worries.

1. Build from a clean checkout of origin/main: `nix build .#ui -o
   result-ui-<label>`, then verify the change is in the bundle
   (`grep -rl "<string only the new code contains>" result-ui-<label>/assets`).
2. Tar it up, scp it over, extract to a staging dir (busybox tar:
   `gunzip -c x.tgz | tar -C <staging> -xf -`), sanity-check
   `<staging>/index.html` exists.
3. Swap: `mv ui ui.pre-<change> && mv ui.new ui` (keep the `ui.pre-<change>`
   backup for instant revert).
4. Verify the served page references the new hashed bundle:
   `curl -s http://<pod>:3000/ | grep -o "index-[A-Za-z0-9]*\.js"` and that
   the asset returns 200.

## Procedure — deploying an edited config

**podd rewrites `config.ron` itself** on any prime/settings save (the
`POST /settings` primePodDaily bridge, MQTT `set_prime`/`set_away_mode`,
presence calibration): it serde-round-trips the whole file, which **strips
every hand-written comment** and normalizes formatting (observed 2026-08-23;
verified it does NOT inject alarm blocks). Don't be surprised by a
comment-free config, don't treat the rewrite as corruption, and keep the
commented original as a `config.ron.pre-<change>` backup before triggering
any such save. Hand-edits that matter long-term belong in a backup or the
repo, not only in on-device comments.

1. `scp` the live `config.ron` down to your workstation.
2. Edit it locally.
3. **Before pushing anything back, diff the alarm blocks against what's
   live.** On 2026-07-20 a migrated config silently armed alarms that had
   been off — that's not a hypothetical, it happened on this exact bed. Any
   `alarm:` block appearing, changing, or losing an explicit "off" between
   the live config and your edited one is a stop-and-confirm-with-Jeremy
   moment, not a push-and-see moment.
4. **Back up the on-device config first**, using the
   `config.ron.pre-<change>-fix` naming convention (e.g.
   `config.ron.pre-timezone-fix`) — pick `<change>` to describe what you're
   about to do, so a future reader (or you, later) knows why the backup
   exists.
5. `scp` the edited config back to the live config path (confirm the current
   path in `CLAUDE.local.md` — seen as `/data/podd/config.ron` live even
   though the release installer documents `/opt/podd/config.ron`; these are
   not always the same path).
6. `systemctl restart podd` and verify via journal as in binary-deploy step 6.

## Failure modes to know about

- **The 60-second sensor-MCU zombie window.** Per the root `CLAUDE.md`
  safety tripwires: the sensor MCU is a zombie for ~60 s after *any* podd
  restart — it streams telemetry and answers Ping, but **silently ignores
  alarm/actuation writes** during that window. If you're testing an
  actuation-path change, `systemctl is-active` returning `active` at 12 s
  tells you the process started; it tells you nothing about whether an
  alarm/setpoint write in the first minute actually landed. Don't call a
  test "passed" from journal output alone inside that window — wait it out,
  or look for the write being retried/confirmed rather than fire-and-forget
  evidence.
- **`PODD_DRY_RUN` semantics can silently flip.** A fresh binary or a fresh
  config does not change the drop-in that overrides dry-run — but a fresh
  *systemd unit file* (if you ever deploy `install/podd.service` itself)
  could remove that override outright, changing whether writes are real
  without anyone touching `config.ron`. If you deploy a new unit file, check
  dry-run behavior explicitly afterward instead of assuming it's unchanged.
- **LSP framing has no byte-stuffing.** A `0x7E` byte anywhere in a command
  payload gets the whole frame silently dropped by the frozen MCU — this is
  why setpoints go through delimiter-safe nudging rather than raw encoding
  (`FrozenTarget::delimiter_safe` in `pod-proto`). If a config/setpoint
  change you pushed seems to just not take effect with no error anywhere,
  this is a candidate cause, not just "the write didn't land."
- **Busybox tar has no `-z`.** The on-device tar is busybox: `tar xzf`
  fails mid-script (`invalid option -- 'z'`), and `-a` (by-extension) has
  failed on a `.tgz` too ("invalid tar magic"). When shipping the UI bundle,
  extract with `gunzip -c bundle.tgz | tar -C <dir> -xf -`. Bit a deploy
  2026-08-16 — podd sat stopped while the tar step failed, because the swap
  script did backup→stop→install→extract in one `set -e` block. Do the UI
  extraction to a staging dir BEFORE `systemctl stop podd`, then swap dirs.
- **`printf '%s'` does not expand escapes.** If you're scripting any of the
  scp/install steps above and generating text on-device (fstab lines,
  config snippets), a literal `\t`/`\n` from `printf '%s'` has previously
  taken out a mount. Use `printf '%b'`, `echo`, or literal characters.
- **Concurrent sessions deploy too.** More than one agent session can be
  working this repo at once, and deploys have landed minutes apart
  (2026-08-16: two deploys 03:24:57 and 03:26:52 UTC). After your swap,
  `sha256sum` the installed binary and compare against your build — Nix
  builds are reproducible, so a mismatch means someone else's build is live,
  not a corrupted copy. Check `origin/main` for newer merges before assuming
  anything is wrong: a later build of a descendant commit still contains
  your change and needs no re-deploy.
- **Silent no-op restarts.** `systemctl restart podd` succeeding and the
  journal showing a clean startup does not by itself prove the *new* binary
  or config is what's running — confirm you actually replaced the file at
  the path systemd's `ExecStart` points to (`systemctl cat podd` shows the
  unit's `ExecStart=`), especially after a path convention has drifted.
