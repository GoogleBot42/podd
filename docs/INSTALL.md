# Installing podd

**Who this is for:** anyone with root or SSH access to their Pod — via
[FLASHING.md](FLASHING.md), or because you already run free-sleep / opensleep.
You need a root shell on the Pod and the Pod on your network.

> Start with the **userland install** below: one command, writes no disk blocks,
> backs you up first, and is fully reversible. The
> [A/B slot install](#advanced-ab-slot-install) is for those who want podd's own
> OS image and accept the trade-offs.

---

## TL;DR — quickest path

From a root shell on the Pod:

```sh
curl -fsSL https://raw.githubusercontent.com/GoogleBot42/podd/main/install/install.sh \
  | sh -s -- --source github:GoogleBot42/podd
```

Then open `http://<pod-ip>:3000` in a browser. podd is installed, Eight's stack
is disabled, and podd starts on boot. It runs in dry-run mode — logging the
hardware writes it would make without driving the hardware — until you
deliberately arm it. See [Arming real control](#arming-real-hardware-control).

Related: **[FLASHING.md](FLASHING.md)** (getting root) ·
**[UPDATING.md](UPDATING.md)** (keeping it current) ·
**[RECOVERY.md](RECOVERY.md)** (undo / recovery).

---

## The one-liner, explained

The command does not pipe an unverified installer into the shell. The bootstrap
`install.sh` downloads the real installer `podd-install.sh`, prints that script's
SHA-256 so you can compare it against the value in the release notes, and only
then runs it.

For a fail-closed install, pin the digest so a mismatch aborts:

```sh
curl -fsSL https://raw.githubusercontent.com/GoogleBot42/podd/main/install/install.sh \
  | PODD_INSTALL_SHA256=<value-from-release-notes> sh -s -- --source github:GoogleBot42/podd
```

`PODD_INSTALLER_URL=<url>` overrides where the installer is fetched from. The
bundle the installer then installs is separately SHA-256-verified (and optionally
signature-verified) against the release manifest, independently of how the
installer itself was fetched.

---

## Choosing a release source

`podd-install.sh` (and the one-liner, via `-- <args>`) accepts several source forms:

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

Each has a matching env var (`PODD_RELEASE_SOURCE`, `PODD_RELEASE_URL`,
`PODD_RELEASE_DIR`) for use with the one-liner.

Flags:

| Flag | Env var | Meaning |
|---|---|---|
| `--variant pod4` \| `pod3` | `PODD_VARIANT` | Which default config to seed. Default is `pod4` — pass `--variant pod3` on a Pod 3. |
| `--channel NAME` | `PODD_CHANNEL` | Expected release channel (default `stable`); warns on mismatch. |
| `--pubkey PATH` | `PODD_PUBKEY` | Require a valid signature from this key (see [signatures](#signatures-optional-and-owner-controlled)). |
| `--prefix DIR` | `PODD_PREFIX` | Install root, default `/opt/podd`. |
| `--no-mask` | | Do not disable/mask Eight's vendor services. |
| `--no-muzzle` | `PODD_NO_MUZZLE=1` | Do not install the [egress muzzle](#the-egress-muzzle-no-phoning-home) firewall. |
| `--no-start` | | Install but don't enable/start `podd.service`. |

---

## What the installer actually does

`podd-install.sh` is POSIX-sh / busybox-friendly, idempotent, and safe to re-run.

1. **Backs up first** to `/opt/podd/backup/<timestamp>/`: U-Boot environment
   (`fw_printenv.txt`), active A/B slot pointer (`active-slot.txt`), the partition
   list, and a read-only copy of the eMMC MBR. It **never writes** to the eMMC
   block devices.
2. **Fetches the manifest + app artifact** from your chosen source (the app is the
   `app-<version>.squashfs` named in `manifest.json`).
3. **Verifies integrity — always.** It checks the artifact's exact size and
   SHA-256 against the manifest and refuses to install on a mismatch. A signature
   is checked only if you pass `--pubkey`.
4. **Installs the payload** under `/opt/podd/releases/<version>/rootfs/` and
   atomically flips the `/opt/podd/current` symlink to it, so the binary is always
   at `/opt/podd/current/rootfs/podd`. This is the same on-device layout the OTA
   agent uses, so hand-installs and auto-updates are interchangeable.
5. **Seeds a default config the first time only:** copies `config.<variant>.ron`
   to `/opt/podd/config.ron` if that file doesn't exist. It never overwrites your
   edits.
6. **Installs the systemd unit** to `/etc/systemd/system/podd.service`.
7. **Masks Eight's vendor stack** (unless `--no-mask`) so it cannot interfere with
   podd or revert the unit to stock: `swupdate`, `swupdate.socket`,
   `swupdate-progress`, `defibrillator`, `dac`, `frank`, `capybara`, `telegraf`,
   `vector`, `frankenfirmware`, `eight-kernel`. It **never touches `cage`** (your
   persistent data partition).
8. **Installs the [egress muzzle](#the-egress-muzzle-no-phoning-home)** (unless
   `--no-muzzle`) — a default-DROP firewall (`podd-muzzle.service`) so the Eight
   Sleep software still on disk can never phone home.
9. **Enables and starts `podd.service`** (unless `--no-start`).

It finishes by printing the binary path, config path, backup location, and UI URL.

---

## The egress muzzle (no phoning home)

A userland install leaves Eight Sleep's software on disk — masked, not removed,
which is what makes it reversible. Masking alone is not a guarantee: vendor
watchdogs and a stock OTA slot flip can re-awaken "disabled" services. The
installer therefore also ships `podd-muzzle.service`, a default-DROP `iptables`
firewall that blocks outbound contact regardless of what wakes up.

- **Inbound:** loopback + your LAN only.
- **Outbound:** loopback, replies, your LAN, DHCP, and NTP (udp/123) anywhere —
  the Pod has no RTC battery, podd refuses to arm alarms until time syncs, and
  most home routers don't serve NTP.
- **Everything else is dropped**, including IPv6 to the open internet and DNS to
  WAN resolvers — a public resolver would leak every vendor hostname lookup, and
  DHCP already points the Pod at your router's resolver.

The rules live in `/etc/podd/muzzle/*.rules` — edit them (e.g. to allow a
Tailscale range) and `systemctl restart podd-muzzle` to apply.

**The trade-off:** a muzzled Pod cannot reach GitHub, so `podd-install.sh`
re-runs and the on-device update agent must use a LAN release source
(`--url http://<lan-host>/...`, `--dir`, or a LAN Gitea) — or lift the muzzle
for the duration of the download:

```sh
systemctl stop podd-muzzle     # firewall open (until started again / reboot)
podd-install.sh --source github:GoogleBot42/podd
systemctl start podd-muzzle    # muzzled again
```

This applies to stock-rootfs installs only. podd's own clean-room OS image is
built entirely from source and has nothing to muzzle; there it runs a LAN-only
*inbound* firewall and leaves egress open so OTA updates work unchanged.

---

## Reaching the UI

podd serves a web UI and a REST API (with SSE log streaming) on port 3000, bound
to all interfaces: `http://<pod-ip>:3000`. Find `<pod-ip>` from your router, or
read it from the installer's closing summary. Health and status are available at
`http://<pod-ip>:3000/api/serverStatus`.

---

## Dry-run by default (and arming real hardware control)

For safety, `podd.service` ships with `Environment=PODD_DRY_RUN=true`. While this
is `true`, podd logs the MCU/hardware writes it would make but does **not** drive
the Pod's heating, pump, or MCUs. This lets you install podd, exercise the UI, and
verify the setup before it controls the hardware.

### Arming real hardware control

Disable dry-run only when you are ready for podd to drive the hardware. Use a
systemd drop-in so an update never resets the setting:

```sh
sudo systemctl edit podd
```

Add:

```ini
[Service]
Environment=PODD_DRY_RUN=false
```

Then restart and check the service:

```sh
sudo systemctl restart podd
systemctl status podd
journalctl -u podd -f
```

Edit your settings in `/opt/podd/config.ron` (temperatures, schedule, LED, MQTT,
etc.), then `systemctl restart podd` to apply. The MQTT broker link (host, port,
credentials, on/off) is also editable from the web UI under **Settings → MQTT**;
it writes the same `config.ron` block and takes effect on the next restart.

---

## Signatures: optional and owner-controlled

podd separates integrity from authenticity:

- **Integrity (SHA-256) is always enforced.** A corrupted or tampered artifact is
  always rejected.
- **Authenticity (a signature) is your choice:** run unsigned (the default
  without `--pubkey`, suitable for your own builds), trust the project's key, or
  generate and trust your own key
  (see [UPDATING.md](UPDATING.md#3-trust-your-own-key)).

To require a valid signature at install time, pass the verifying key:

```sh
podd-install.sh --source github:owner/repo --pubkey /path/to/signing.pub
```

Behavior with `--pubkey`:

- **Signature valid** → installs, prints "authenticity verified".
- **Signature invalid** → **refuses to install.**
- **No verifier available on the device** (no `podup`, no `jq`+`openssl>=3`, no
  `minisign`) → it warns that the signature could not be checked but proceeds on
  SHA-256 integrity, and reminds you to set `PODD_UPDATER_TRUST` for future
  auto-updates.

Without `--pubkey`, the installer warns that only integrity was checked.

> To enforce signatures on future auto-updates as well, set the trust policy for
> the on-device update agent (`PODD_UPDATER_TRUST=/path/to/signing.pub`). See
> [UPDATING.md](UPDATING.md).

---

## Advanced: A/B slot install

> **This install writes to eMMC.** Unlike the userland install, a mistake here
> can require bootloader-level recovery (serial U-Boot on MediaTek; JTAG or the
> SD nets on the i.MX SD hub). Use it only if you want podd's own full OS image
> rather than the userland payload, and only after reading
> [RECOVERY.md](RECOVERY.md). Otherwise use the userland install above.

`podd-slot-install.sh` installs podd's own rootfs into the **inactive** eMMC A/B
slot, leaving your currently-running (stock or podd) slot untouched as a rollback
target, then flips the U-Boot pointer with the rollback state machine armed. If
the new slot fails to boot 3 times, U-Boot's `altbootcmd` automatically reverts to
the old slot. It never touches the active slot and never touches `mmcblk2p3`
(`cage`, your persistent data).

The payload is `podd-rootfs.tar.gz`, published with each release (and carried on
the recovery SD — see [RECOVERY.md](RECOVERY.md)). Download it next to its
`.sha256`, or point the script at the URL and it fetches both.

```sh
# Install podd's rootfs to the inactive slot (path or URL):
podd-slot-install.sh --rootfs /path/podd-rootfs.tar.gz
podd-slot-install.sh --rootfs https://host/podd-rootfs.tar.gz

# After it reboots into the new slot and podd comes up healthy, confirm it
# so the rollback is disarmed and this slot becomes permanent:
podd-slot-install.sh --confirm-good
```

With no `--rootfs` it searches the usual locations (the recovery SD's
`/data/podd-recovery/`, `/opt/images/Yocto/`, `/opt/podd/`, next to the script,
the current directory) and reports every path it tried if it finds nothing.
An adjacent `<tarball>.sha256` is verified automatically; pass `--sha256 <hex>`
to override it.

> **This runs on a rooted stock system**, whose U-Boot env has `mmcdev=2` so
> that `mmcpart` selects an eMMC slot. Booted from podd's own SD image `mmcdev=1`
> and `mmcpart` selects a slot on the card, so flipping it after writing eMMC
> would repoint U-Boot at the wrong device — the script checks `mmcdev` and
> refuses. (From podd's SD you already have podd; OS updates there go through
> pod-updater — [UPDATING.md](UPDATING.md).)

Other flags: `--disk DEV` (override the auto-detected eMMC whole-disk device),
`--no-reboot`, `--yes` (skip the interactive `YES` prompt). It backs up the env +
MBR to `/opt/podd/backup/slot-<timestamp>/` before writing.

---

Next: **[UPDATING.md](UPDATING.md) — keep podd up to date.**
