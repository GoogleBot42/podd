# Keeping podd updated

For anyone with podd already installed ([INSTALL.md](INSTALL.md)) who wants to
configure how it stays up to date. You'll need root/SSH on the Pod. Covers the
built-in over-the-air (OTA) update agent: release sources, trust policy
(including trusting your own signing key), and rollback.

Related: **[INSTALL.md](INSTALL.md)** (first install) ·
**[RELEASING.md](RELEASING.md)** (cutting releases) ·
**[RECOVERY.md](RECOVERY.md)** (if something goes wrong).

---

## How updates work

podd ships with an on-device update agent built into the `podd` daemon. It
periodically checks a release source you configure, verifies any new release,
and — depending on your settings — either applies it or reports that one is
available.

- **App updates are atomic and reversible.** Each release is unpacked under
  `/opt/podd/releases/<version>/` and a `current` symlink is flipped to activate
  it. The agent keeps the last few releases (default 3) so it can flip back
  immediately. On the clean-room OS image the same layout lives at
  `/data/podd/updates/` instead (set via `PODD_UPDATER_*` in the shipped
  `podd.service`): the persistent `/data` partition survives OS A/B slot swaps,
  and the `podd-launch` wrapper execs the active release — falling back to the
  OS-baked `/usr/bin/podd` whenever no release is installed, so a broken release
  chain cannot leave the Pod without a daemon.
- **Every update is integrity-checked (SHA-256).** Whether a signature is also
  required is configurable (see [Trust policy](#trust-policy)).
- **A new release is health-checked before it's trusted.** After activating an app
  update, podd queries its own health endpoint
  (`http://127.0.0.1:3000/api/serverStatus` by default) within ~20s; if it
  doesn't come up healthy, it rolls back.
- **Defaults are conservative.** `PODD_UPDATER_ENABLED=true`, with Manual mode
  and dry-run for destructive OS/MCU writes.

---

## Auto vs Manual

Set with `PODD_UPDATER_MODE=auto` (or `manual`).

| Mode | Behavior |
|---|---|
| **Manual** (default) | Polls and reports that an update is available; you apply it explicitly (the panel's **Update to …** button, or the installer). |
| **Auto** | Polls and applies auto-appliable updates (app tier). |

---

## Configuring the update source + trust (systemd drop-in)

The agent reads its configuration from `PODD_UPDATER_*` environment variables.
Set them in a systemd drop-in so they survive updates and service reinstalls:

```sh
sudo systemctl edit podd
```

Example (the project's GitHub releases, auto-updating, trusting your own key):

```ini
[Service]
# Where to fetch releases from (see the source table below):
Environment=PODD_UPDATER_GITHUB=GoogleBot42/podd
# Apply updates automatically (omit for the default: manual/report-only):
Environment=PODD_UPDATER_MODE=auto
# Check hourly (value is in seconds; 3600 is the default):
Environment=PODD_UPDATER_POLL_SECS=3600
# Trust policy: require a valid signature from this key (see below):
Environment=PODD_UPDATER_TRUST=/opt/podd/keys/signing.pub
```

Then restart:

```sh
sudo systemctl restart podd
journalctl -u podd -f      # watch it poll / apply
```

### Source options

Configure one or more sources; they are tried in order until one yields a
verified manifest.

| Variable | Value | Resolves to |
|---|---|---|
| `PODD_UPDATER_GITHUB` | `owner/repo` or `owner/repo@vX.Y.Z` | GitHub Releases (`.../releases/latest/download/…` or a pinned tag). |
| `PODD_UPDATER_GITEA` | `https://host/owner/repo` or `…@vX.Y.Z` | Gitea/Forgejo Releases on a self-hosted host. |
| `PODD_UPDATER_MANIFEST_URL` + `PODD_UPDATER_ARTIFACT_BASE` | explicit URLs | Any host serving `manifest.json` + artifacts. |
| `PODD_UPDATER_LOCAL_DIR` | a directory path | Offline / LAN mount / USB stick holding the release. |

> **Muzzled stock installs:** if `podd-install.sh` installed its egress muzzle
> (the default on a stock rootfs — see
> [INSTALL.md](INSTALL.md#the-egress-muzzle-no-phoning-home)), GitHub and any
> other WAN source are unreachable by design. Use a LAN source (Gitea, an HTTP
> host, or `PODD_UPDATER_LOCAL_DIR`), or `systemctl stop podd-muzzle` for the
> duration of the update and `start` it again after.

Other settings:

| Variable | Default | Meaning |
|---|---|---|
| `PODD_UPDATER_ENABLED` | `true` | `false`/`0` disables the agent entirely. |
| `PODD_UPDATER_CHANNEL` | `stable` | Which release channel to follow (a channel switched from the UI persists an override that wins over this — see [Seeing what the agent is doing](#seeing-what-the-agent-is-doing)). |
| `PODD_UPDATER_MODE` | `manual` | `auto` or `manual`. |
| `PODD_UPDATER_POLL_SECS` | `3600` | Poll interval, seconds. |
| `PODD_UPDATER_KEEP` | `3` | How many recent releases to retain for rollback (min 1). |
| `PODD_UPDATER_TRUST` | `unsigned` | `unsigned`, or comma-separated pubkey file paths (see below). |
| `PODD_UPDATER_OS_DRY_RUN` | `true` | `false`/`0` arms live OS-image writes to the inactive A/B slot. |
| `PODD_UPDATER_OS_WRITER` | `auto` | `auto` (live writer only when `/etc/fw_env.config` + the slot devices exist), `dry`, or `mmc`. |
| `PODD_UPDATER_MCU_DRY_RUN` | `true` | `false`/`0` arms live MCU firmware flashes. |
| `PODD_UPDATER_HEALTH_URL` | `http://127.0.0.1:3000/api/serverStatus` | Health check for the canary. |

> The `*_DRY_RUN` gates default to `true` (safe). Leave them on until you are
> deliberately ready for podd to write the OS image or flash the MCUs. This is
> separate from the app-level `PODD_DRY_RUN` in [INSTALL.md](INSTALL.md), which
> gates driving the hardware at runtime.

---

## Trust policy

Signing is optional and owner-controlled; there is no central authority. Three
options:

### 1. Run unsigned (integrity only)

```ini
Environment=PODD_UPDATER_TRUST=unsigned
```

The default. Every artifact is still SHA-256-verified; only authenticity is
skipped.

### 2. Trust the project's key

Obtain the project's `signing.pub` (published as a release asset when a signed
release is cut), put it on the device, and point trust at it:

```ini
Environment=PODD_UPDATER_TRUST=/opt/podd/keys/signing.pub
```

Only releases signed by that key are then accepted.

### 3. Trust your own key

Sign releases with a key you generate and have the device trust only that key.
This is the fully self-hosted path, with no third-party trust.

<a name="trust-your-own-key"></a>

```sh
# On your workstation — generate a keypair (keep signing.key offline):
podup keygen --out-dir keys
#   -> keys/signing.key   (SECRET — never publish, never put on the Pod)
#   -> keys/signing.pub   (public — safe to distribute)
#   prints a key_id for reference

# Build + sign a release with it (see RELEASING.md for the full flow):
podup release --channel stable --key keys/signing.key --out-dir dist \
    --app-src <built-app-dir> --app-version 0.1.0

# Copy ONLY the public key to the Pod and trust it:
scp -P 8822 keys/signing.pub rewt@<pod-ip>:/opt/podd/keys/signing.pub
```

Then set `Environment=PODD_UPDATER_TRUST=/opt/podd/keys/signing.pub` on the Pod,
as in option 2. Multiple trusted keys can be listed comma-separated:
`PODD_UPDATER_TRUST=/opt/podd/keys/a.pub,/opt/podd/keys/b.pub`.

> The private `signing.key` never goes on the device; the Pod only verifies.
> Keep it offline.

To verify a release by hand before trusting it:

```sh
podup verify --pubkey keys/signing.pub --manifest dist/manifest.json --dir dist
```

---

## Seeing what the agent is doing

The web UI has an **Updates** panel under **Settings**: installed version per
tier, agent enabled/mode, when it last checked and whether that check succeeded,
anything the channel is offering, the last error, and the last release applied.
Its controls: **Check now** (poll out of band), **Update to \<version\>**
(install the offered app release; shown only when one is offered), **Roll back**
(return to the previous app release), and the **Release channel** selector.
Installing and rolling back both restart podd.

The same data is available over HTTP:

```sh
curl -s http://<pod-ip>:3000/api/updates          # status
curl -s -X POST http://<pod-ip>:3000/api/updates/check
curl -s -X POST -H 'content-type: application/json' -d '{"kind":"app"}' \
    http://<pod-ip>:3000/api/updates/apply
curl -s -X POST -H 'content-type: application/json' -d '{"channel":"beta"}' \
    http://<pod-ip>:3000/api/updates/channel
curl -s -X POST http://<pod-ip>:3000/api/updates/rollback
```

`"updater": null` means no update agent is running (it failed to build — check
`journalctl -u podd` for a trust-policy error). This is not the same as being up
to date.

**Applying** is the app tier only: podd verifies and stages the release, flips
`current`, and restarts into it as a canary that commits itself or is rolled
back automatically (see [How rollback works](#how-rollback-works)). That restart
usually kills the HTTP request before it can answer; a dropped connection means
podd is restarting, and `GET /api/updates` is authoritative once it is back.
Applying the OS or MCU tiers answers `501`: those live paths are behind their
dry-run gates, so apply them with the installer instead. With the agent switched
off (`PODD_UPDATER_ENABLED=false`) an apply is refused rather than silently
ignored.

**Switching channels** takes effect immediately — no restart — and is
persisted on the Pod as `<release-root>/channel.json` (by default
`/opt/podd/releases/channel.json`). That override outranks
`PODD_UPDATER_CHANNEL`. To go back to following the env var, delete that file
and restart podd. Switching applies nothing on its own and drops the previous
channel's offers — press **Check now** afterwards.

---

## Updating manually

If the agent can't reach the channel, is switched off, or you want a specific
version, re-run the installer. It does the same verify-then-activate flow and is
also how the OS and MCU tiers are applied:

```sh
curl -fsSL https://raw.githubusercontent.com/GoogleBot42/podd/main/install/install.sh \
  | sh -s -- --source github:GoogleBot42/podd
```

Or pin a tag:

```sh
podd-install.sh --source github:GoogleBot42/podd@v0.1.0
```

Re-running is idempotent: it keeps `/opt/podd/config.ron` and your systemd
drop-ins, swapping in the new release under `current`.

---

## How rollback works

Two layers, depending on what was updated:

**App updates (the common case).** The agent keeps the last `PODD_UPDATER_KEEP`
(default 3) releases. Activating one is a two-phase trial, because the restart
kills the process doing the update: the old podd flips `current` and restarts;
the new podd health-checks its own API and either commits the release or flips
`current` back to the previous release and restarts again. A release that
crashes before it can serve gets 3 boot attempts (counted at startup, like
U-Boot's `bootlimit`) before the same rollback happens. A rolled-back version is
remembered and never auto-retried — apply it manually to try again. The previous
release directory remains in place either way.

**OS / slot updates (i.MX A/B).** On the clean-room SD image, applying an OS
update streams the release's `os-<version>.ext4.zst` onto the inactive slot,
verifies the write by reading it back, and arms the U-Boot rollback state
machine (`upgrade_available=1 bootcount=0 ustate=1` plus the slot flip). It then
waits for a reboot; podd never reboots the Pod on its own. Rollback then happens
in the bootloader itself:

- On each armed boot U-Boot increments `bootcount` before trying the new slot.
  If it fails to boot 3 times (`bootlimit=3`), U-Boot flips the pointer back to
  the previous, known-good slot in the same power cycle, with no user action.
  (Exact env-var semantics live in the state-machine comment block in
  `os/board/eightsleep/imx8mm-varsom/uboot-env.txt`.)
- Once podd boots healthy on the new slot it confirms the slot good
  automatically (disarms the env and records the new OS version), even when
  update polling is disabled. `podd-slot-install.sh --confirm-good` is the
  manual equivalent for the stock-U-Boot eMMC install path (see
  [INSTALL.md](INSTALL.md#advanced-ab-slot-install)).

To force a rollback manually, or if an update leaves you unable to boot, see
**[RECOVERY.md](RECOVERY.md)** — the shortest fix is usually a one-line
`fw_setenv`/`setenv` at the serial U-Boot prompt.
