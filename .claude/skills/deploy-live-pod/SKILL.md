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

- **Confirm with Jeremy before restarting podd, pushing a config, or
  power-cycling** — this is his real bed, not a lab box. He'll do the
  "is that fine?" round-trip and can run physical tests (e.g. double-tap
  dismissal) when given a clear protocol.
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

## Procedure — deploying an edited config

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
