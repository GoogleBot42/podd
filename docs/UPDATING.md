# Keeping podd updated

**Who this is for / what you'll need:** Anyone with podd already installed
([INSTALL.md](INSTALL.md)) who wants to understand and configure how it stays up to
date. You'll need root/SSH on the Pod. No new hardware. This guide explains the
built-in over-the-air (OTA) update agent, how to point it at a release source, how
to choose your trust policy (including trusting your *own* signing key), and how
rollback keeps a bad update from bricking you.

Related: **[INSTALL.md](INSTALL.md)** (first install) ·
**[RELEASING.md](RELEASING.md)** (cutting releases if you're a maintainer) ·
**[RECOVERY.md](RECOVERY.md)** (if something goes wrong).

---

## How updates work, in plain language

podd ships with an on-device **update agent** (built into the `podd` daemon). It
periodically checks a release source you configure, verifies any new release, and —
depending on your settings — either applies it automatically or just tells you one
is available.

Key ideas:

- **App updates are atomic and reversible.** Each release is unpacked into its own
  directory under `/opt/podd/releases/<version>/`, and a `current` symlink is
  flipped to activate it. The agent keeps the last few releases (default **3**) so
  it can flip back instantly.
- **Every update is integrity-checked (SHA-256), always.** Whether a signature is
  *also* required is your choice (see [Trust policy](#trust-policy)).
- **A new release is health-checked before it's trusted.** After activating an app
  update, podd hits its own health endpoint
  (`http://127.0.0.1:3000/api/serverStatus` by default) within a timeout (~20s). If
  the new version doesn't come up healthy, it rolls back.
- **The agent is enabled but conservative by default:** `PODD_UPDATER_ENABLED=true`,
  but **Manual** mode and **dry-run** for destructive OS/MCU writes. It won't
  surprise you.

---

## Auto vs Manual

| Mode | Behavior |
|---|---|
| **Manual** (default) | Polls and *reports* that an update is available; you apply it explicitly. |
| **Auto** | Polls and applies auto-appliable updates (app tier) on its own. |

Set the mode with `PODD_UPDATER_MODE=auto` (or `manual`).

---

## Configuring the update source + trust (systemd drop-in)

The agent reads its configuration from `PODD_UPDATER_*` environment variables. The
clean way to set them is a **systemd drop-in**, so your settings survive updates and
service reinstalls:

```sh
sudo systemctl edit podd
```

Add a block like this (a Gitea example, auto-updating, trusting your own key):

```ini
[Service]
# Where to fetch releases from (pick ONE source form; see the table below):
Environment=PODD_UPDATER_GITEA=https://git.neet.dev/zuckerberg/podd
# Apply updates automatically (omit for the default: manual/report-only):
Environment=PODD_UPDATER_MODE=auto
# Check hourly (value is in seconds; 3600 is the default):
Environment=PODD_UPDATER_POLL_SECS=3600
# Trust policy: require a valid signature from this key (see below):
Environment=PODD_UPDATER_TRUST=/opt/podd/keys/signing.pub
```

Then reload and restart:

```sh
sudo systemctl restart podd
journalctl -u podd -f      # watch it poll / apply
```

### Source options

Configure **one or more** sources (they're tried in order until one yields a
verified manifest):

| Variable | Value | Resolves to |
|---|---|---|
| `PODD_UPDATER_GITHUB` | `owner/repo` or `owner/repo@vX.Y.Z` | GitHub Releases (`.../releases/latest/download/…` or a pinned tag). |
| `PODD_UPDATER_GITEA` | `https://host/owner/repo` or `…@vX.Y.Z` | Gitea/Forgejo Releases on a self-hosted host. |
| `PODD_UPDATER_MANIFEST_URL` + `PODD_UPDATER_ARTIFACT_BASE` | explicit URLs | Any host serving `manifest.json` + artifacts. |
| `PODD_UPDATER_LOCAL_DIR` | a directory path | Offline / LAN mount / USB stick holding the release. |

Other knobs:

| Variable | Default | Meaning |
|---|---|---|
| `PODD_UPDATER_ENABLED` | `true` | `false`/`0` disables the agent entirely. |
| `PODD_UPDATER_CHANNEL` | `stable` | Which release channel to follow. |
| `PODD_UPDATER_MODE` | `manual` | `auto` or `manual`. |
| `PODD_UPDATER_POLL_SECS` | `3600` | Poll interval, seconds. |
| `PODD_UPDATER_KEEP` | `3` | How many recent releases to retain for rollback (min 1). |
| `PODD_UPDATER_TRUST` | `unsigned` | `unsigned`, or comma-separated pubkey file paths (see below). |
| `PODD_UPDATER_OS_DRY_RUN` | `true` | `false`/`0` arms live OS-image writes to the inactive A/B slot. |
| `PODD_UPDATER_OS_WRITER` | `auto` | `auto` (live writer only when `/etc/fw_env.config` + the slot devices exist), `dry`, or `mmc`. |
| `PODD_UPDATER_MCU_DRY_RUN` | `true` | `false`/`0` arms live MCU firmware flashes. |
| `PODD_UPDATER_HEALTH_URL` | `http://127.0.0.1:3000/api/serverStatus` | Health check for the canary. |

> The `*_DRY_RUN` gates default to **true (safe)**. Leave them on until you're
> deliberately ready for podd to write the OS image or flash the MCUs. This is
> separate from the app-level `PODD_DRY_RUN` in [INSTALL.md](INSTALL.md), which
> gates driving the hardware at runtime.

---

## Trust policy

Signing is **optional and entirely owner-controlled** — there is no central
authority. You have three choices:

### 1. Run unsigned (integrity only)

```ini
Environment=PODD_UPDATER_TRUST=unsigned
```

Every artifact is still SHA-256-verified; only *authenticity* is skipped. Fine for
your own builds on your own device. This is the default.

### 2. Trust the project's key

Obtain the project's `signing.pub` (published as a release asset when a signed
release is cut), put it on the device, and point trust at it:

```ini
Environment=PODD_UPDATER_TRUST=/opt/podd/keys/signing.pub
```

Now only releases signed by that key are accepted.

### 3. Trust your OWN key

You can sign releases with a key **you** generate and have the device trust only
that. This is the fully self-hosted, no-third-party-trust path.

<a name="trust-your-own-key"></a>

```sh
# On your workstation — generate a keypair (keep signing.key OFFLINE):
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

Then on the Pod:

```ini
Environment=PODD_UPDATER_TRUST=/opt/podd/keys/signing.pub
```

You can list **multiple** trusted keys, comma-separated:
`PODD_UPDATER_TRUST=/opt/podd/keys/a.pub,/opt/podd/keys/b.pub`.

> The private `signing.key` **never** goes on the device — the Pod only ever
> *verifies*. Keep it offline.

To verify a release by hand before trusting it:

```sh
podup verify --pubkey keys/signing.pub --manifest dist/manifest.json --dir dist
```

---

## Seeing what the agent is doing

The web UI has an **Updates** panel under **Settings**: installed version per
tier, whether the agent is enabled and in auto or manual mode, when it last
checked (and whether that check succeeded), anything the channel is offering,
the last error, and the last thing applied. Two buttons there drive the agent —
**Check now** (poll the channel out of band) and **Roll back** (return to the
previous app release; podd restarts).

The same data is available over HTTP if you'd rather script it:

```sh
curl -s http://<pod-ip>:3000/api/updates          # status
curl -s -X POST http://<pod-ip>:3000/api/updates/check
curl -s -X POST http://<pod-ip>:3000/api/updates/rollback
```

`"updater": null` means no update agent is running (it failed to build — check
`journalctl -u podd` for a trust-policy error); that is *not* the same as
"you're up to date". The release **channel is shown read-only**: it is read
once at start-up from `PODD_UPDATER_CHANNEL`, so change it in the systemd
drop-in above and restart. There is no "apply this update" button yet — in
Manual mode, apply via the installer (below).

---

## Updating manually

If you're in Manual mode (or just want to force an update now), re-running the
installer is the simplest path — it's idempotent and does the same
verify-then-activate flow:

```sh
curl -fsSL https://git.neet.dev/zuckerberg/podd/raw/branch/main/install/install.sh \
  | sh -s -- --source gitea:https://git.neet.dev/zuckerberg/podd
```

Or point it at a pinned tag to move to a specific version:

```sh
podd-install.sh --source gitea:https://git.neet.dev/zuckerberg/podd@v0.1.0
```

Re-running is safe: it keeps your `/opt/podd/config.ron` and your systemd drop-ins,
just swapping in the new release under `current`.

---

## How rollback works

Two layers protect you, depending on what got updated:

**App updates (the common case).** The agent keeps the last `PODD_UPDATER_KEEP`
(default 3) releases. Activating one is a two-phase *trial*, because the restart
kills the process doing the update: the old podd flips `current` and restarts;
the **new** podd then health-checks its own API and either commits the release
or flips `current` back to the previous release and restarts again. A release
that crashes before it can even serve gets 3 boot attempts (counted at startup,
like U-Boot's `bootlimit`) before the same rollback happens. A rolled-back
version is remembered and never auto-retried — apply it manually to try again.
Nothing is lost either way — the old release directory is still there.

**OS / slot updates (i.MX A/B).** On the clean-room SD image, applying an OS
update streams the release's `os-<version>.ext4.zst` onto the **inactive** slot,
verifies the write by reading it back, and arms the U-Boot rollback state
machine (`upgrade_available=1 bootcount=0 ustate=1` plus the slot flip) — then
waits for a reboot; nothing reboots your bed on its own. From there rollback
happens in the bootloader itself:

- On each armed boot U-Boot increments `bootcount` before trying the new slot.
  If it fails to boot **3 times** (`bootlimit=3`), U-Boot flips the pointer
  back to the previous, known-good slot in the same power cycle — a hands-off
  rollback. (The exact env-var semantics are owned by the state-machine
  comment block in `os/board/eightsleep/imx8mm-varsom/uboot-env.txt`.)
- Once podd boots healthy on the new slot it **confirms the slot good
  automatically** (disarms the env and records the new OS version) — this
  works even when update polling is disabled. `podd-slot-install.sh
  --confirm-good` remains as the manual equivalent for the stock-U-Boot eMMC
  install path (see [INSTALL.md](INSTALL.md#advanced-ab-slot-install)).

If you ever need to force a rollback manually, or an update leaves you unable to
boot, see **[RECOVERY.md](RECOVERY.md)** — the shortest fix is usually a one-line
`fw_setenv`/`setenv` at the serial U-Boot prompt.
