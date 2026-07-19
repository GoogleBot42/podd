# Installing podd

**Who this is for / what you'll need:** Anyone who already has **root or SSH access
to their Pod** — either because you followed [FLASHING.md](FLASHING.md), or because
you already run free-sleep / opensleep. You'll need a shell on the Pod (as root)
and the Pod on your network. This is the easy part: the recommended install is a
single command that **cannot brick your Pod** and is fully reversible.

> New to this? Do the **userland install** below. It writes no disk blocks, backs
> you up first, and you can undo it. The [A/B slot install](#advanced-ab-slot-install)
> is only for people who want podd's own OS image and understand the trade-offs.

---

## TL;DR — quickest path

From a root shell on the Pod:

```sh
curl -fsSL https://raw.githubusercontent.com/GoogleBot42/podd/main/install/install.sh \
  | sh -s -- --source github:GoogleBot42/podd
```

Then open **`http://<pod-ip>:3000`** in a browser. Done. podd is installed,
Eight's stack is disabled, and podd starts on boot. It runs in **safe dry-run
mode** (it logs what it *would* do to the hardware but doesn't drive it yet) until
you deliberately arm it — see [Arming real control](#arming-real-hardware-control).

Related: **[FLASHING.md](FLASHING.md)** (getting root) ·
**[UPDATING.md](UPDATING.md)** (keeping it current) ·
**[RECOVERY.md](RECOVERY.md)** (undo / unbrick).

---

## The one-liner, explained

The command above is intentionally **not** a blind `curl | bash`. It runs a tiny
bootstrap (`install.sh`) that:

1. Downloads the real installer, `podd-install.sh`.
2. **Prints that script's SHA-256** so you can compare it against the value
   published in the release notes before trusting it.
3. Only then runs it.

For a **fail-closed** install, pin the digest so a mismatch aborts before anything
runs:

```sh
curl -fsSL https://raw.githubusercontent.com/GoogleBot42/podd/main/install/install.sh \
  | PODD_INSTALL_SHA256=<value-from-release-notes> sh -s -- --source github:GoogleBot42/podd
```

You can also override where the installer is fetched from with
`PODD_INSTALLER_URL=<url>`. Regardless of any of this, the *bundle* the installer
actually installs is separately SHA-256-verified (and optionally signature-verified)
against the release manifest — so trust never rests on the fetch alone.

---

## Choosing a release source

`podd-install.sh` (and the one-liner, via `-- <args>`) accepts several source
forms. Use whichever matches where the release lives:

```sh
# GitHub Releases (latest, or pin a tag with @vX.Y.Z):
podd-install.sh --source github:owner/repo
podd-install.sh --source github:owner/repo@v0.1.0

# A self-hosted Gitea / Forgejo instance:
podd-install.sh --source gitea:https://git.example.org/owner/repo
podd-install.sh --source gitea:https://git.example.org/owner/repo@v0.1.0

# Any URL that serves a manifest.json (e.g. your own web host):
podd-install.sh --url https://host/path/to/release

# A local directory (offline / USB stick) holding manifest.json + the artifact:
podd-install.sh --dir /mnt/usb/podd-release
```

Every option also has a matching environment variable
(`PODD_RELEASE_SOURCE`, `PODD_RELEASE_URL`, `PODD_RELEASE_DIR`), handy for the
one-liner.

Useful flags:

| Flag | Env var | Meaning |
|---|---|---|
| `--variant pod4` \| `pod3` | `PODD_VARIANT` | Which default config to seed. **Default is `pod4`** — pass `--variant pod3` on a Pod 3. |
| `--channel NAME` | `PODD_CHANNEL` | Expected release channel (default `stable`); warns on mismatch. |
| `--pubkey PATH` | `PODD_PUBKEY` | Require a valid signature from this key (see [signatures](#signatures-optional-and-owner-controlled)). |
| `--prefix DIR` | `PODD_PREFIX` | Install root, default `/opt/podd`. |
| `--no-mask` | | Do **not** disable/mask Eight's vendor services. |
| `--no-start` | | Install but don't enable/start `podd.service`. |

---

## What the installer actually does

`podd-install.sh` is POSIX-sh / busybox-friendly, idempotent, and safe to re-run.
Step by step:

1. **Backs up first.** It writes `/opt/podd/backup/<timestamp>/` containing your
   U-Boot environment (`fw_printenv.txt`), active A/B slot pointer
   (`active-slot.txt`), the partition list, and a read-only copy of the eMMC MBR.
   It **never writes** to the eMMC block devices.
2. **Fetches the manifest + app artifact** from your chosen source (the app is the
   `app-<version>.squashfs` named in `manifest.json`).
3. **Verifies integrity — always.** It checks the artifact's exact size and
   **SHA-256** against the manifest and refuses to install on a mismatch. A
   *signature* is checked only if you pass `--pubkey` (see below).
4. **Installs the payload** under `/opt/podd/releases/<version>/rootfs/` and
   atomically flips the `/opt/podd/current` symlink to it — so the binary is always
   at `/opt/podd/current/rootfs/podd`. This is the *same* on-device layout the OTA
   agent uses, so hand-installs and auto-updates are interchangeable.
5. **Seeds a default config the first time only.** It copies
   `config.<variant>.ron` to `/opt/podd/config.ron` if that file doesn't already
   exist — it will **never clobber** edits you've made.
6. **Installs the systemd unit** to `/etc/systemd/system/podd.service`.
7. **Masks Eight's vendor stack** (unless `--no-mask`) so it can't fight podd or
   auto-revert you to stock. The masked units are: `swupdate`, `swupdate.socket`,
   `swupdate-progress`, `defibrillator`, `dac`, `frank`, `capybara`, `telegraf`,
   `vector`, `frankenfirmware`, `eight-kernel`. It **never touches `cage`** (your
   persistent data partition).
8. **Enables and starts `podd.service`** (unless `--no-start`).

When it finishes it prints a summary with the binary path, your config path, the
backup location, and the UI URL.

---

## Reaching the UI

podd serves a web UI and a REST API (with SSE log streaming) on port **3000**,
bound to all interfaces:

```
http://<pod-ip>:3000
```

Find `<pod-ip>` from your router, or the installer prints it at the end. Health
and status live under `http://<pod-ip>:3000/api/serverStatus`.

---

## Dry-run by default (and arming real hardware control)

For safety, `podd.service` ships with:

```
Environment=PODD_DRY_RUN=true
```

While this is `true`, podd **logs** the MCU/hardware writes it *would* make but
does **not** actually drive the Pod's heating/pump/MCUs. This lets you install,
poke the UI, and confirm everything looks right without any risk while Eight's
stack is being replaced.

### Arming real hardware control

Only when you're ready for podd to actually drive the hardware, flip the gate off.
The clean way is a systemd drop-in so an update never resets your choice:

```sh
sudo systemctl edit podd
```

Add:

```ini
[Service]
Environment=PODD_DRY_RUN=false
```

Then:

```sh
sudo systemctl restart podd
```

Check it's happy:

```sh
systemctl status podd
journalctl -u podd -f
```

Edit your settings in `/opt/podd/config.ron` (temperatures, schedule, LED, MQTT,
etc.), then `systemctl restart podd` to apply.

---

## Signatures: optional and owner-controlled

podd's trust model is deliberately not "trust a vendor":

- **Integrity (SHA-256) is *always* enforced.** A corrupted or tampered artifact is
  always rejected.
- **Authenticity (a signature) is *your* choice.** You can:
  - run **unsigned** (fine for your own builds — the default if you don't pass
    `--pubkey`),
  - **trust the project's key**, or
  - **generate and trust your OWN key** (see [UPDATING.md](UPDATING.md#trust-your-own-key)).

To require a valid signature at install time, pass the verifying key:

```sh
podd-install.sh --source github:owner/repo --pubkey /path/to/signing.pub
```

Behavior with `--pubkey`:

- **Signature valid** → installs, prints "authenticity verified".
- **Signature invalid** → **refuses to install.**
- **No verifier available on the device** (no `podup`, no `jq`+`openssl>=3`, no
  `minisign`) → it warns that the signature couldn't be checked but proceeds on
  SHA-256 integrity, and reminds you to set `PODD_UPDATER_TRUST` for future
  auto-updates.

Without `--pubkey`, you get a loud warning that only integrity was checked — which
is exactly right for your own unsigned builds.

> **To enforce signatures on all *future* auto-updates too**, set the trust policy
> for the on-device update agent (`PODD_UPDATER_TRUST=/path/to/signing.pub`). See
> [UPDATING.md](UPDATING.md).

---

## Advanced: A/B slot install

> **This one writes to eMMC.** Unlike the userland install, a mistake here *can*
> require bootloader-level recovery (serial U-Boot on MediaTek; JTAG or the SD
> nets on the i.MX SD hub — see [RECOVERY.md](RECOVERY.md)). Only do this if you
> specifically want podd's own
> full OS image (not just the userland payload) and you've read
> [RECOVERY.md](RECOVERY.md). Most people should use the userland install above.

`podd-slot-install.sh` installs podd's own rootfs into the **inactive** eMMC A/B
slot, keeping your currently-running (stock or podd) slot pristine as an **instant
rollback**, then flips the U-Boot pointer with the rollback state machine armed. If
the new slot fails to boot 3 times, U-Boot's `altbootcmd` automatically reverts to
the old slot. It never touches the active slot and never touches `mmcblk2p3`
(`cage`, your persistent data).

```sh
# Install podd's rootfs to the inactive slot (path or URL):
podd-slot-install.sh --rootfs /path/podd-rootfs.tar.gz
podd-slot-install.sh --rootfs https://host/podd-rootfs.tar.gz --sha256 <hex>

# After it reboots into the new slot and podd comes up healthy, confirm it
# so the rollback is disarmed and this slot becomes permanent:
podd-slot-install.sh --confirm-good
```

Other flags: `--disk DEV` (override the auto-detected eMMC whole-disk device),
`--no-reboot`, `--yes` (skip the interactive `YES` prompt). It backs up the env +
MBR to `/opt/podd/backup/slot-<timestamp>/` before writing.

> **Heads-up — not built yet.** The `podd-rootfs.tar.gz` artifact this script
> needs (podd's own OS image, "L2") **is not produced by CI yet.** The script is
> ready and will error clearly if the rootfs is missing, but until that artifact
> exists, the **userland install is the only supported install path.** Track this
> in the project's release notes.

---

Next: **[UPDATING.md](UPDATING.md) — keep podd up to date.**
