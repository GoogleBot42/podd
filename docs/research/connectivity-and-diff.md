# Eight Sleep Pod 3 — Connectivity/Provisioning/UI + Three-Way Rootfs Diff

Workspace: `.../scratchpad/work/{rootfs-original, rootfs-modded, rootfs-ota}`
OS base: Yocto "FSLC FrameBuffer 3.3 (hardknott)", i.MX8M-Mini (Variscite DART) `imx8mm-var-dart-eight`, aarch64.

Version identity (`/etc/rat_version`):
- original: `556615a62a1d1100e5e63dac99560add14689cc5`  (older stock)
- modded (freesleep): `23c67e7723669745ef1e3aef1b543591203f3957`
- ota: `23c67e7723669745ef1e3aef1b543591203f3957`  (**identical to modded**)

**Critical structural finding:** the "modded" (freesleep) rootfs is the **newer OTA image plus 5 files**. Freesleep is deployed *on top of* the newer official firmware, not the original. So "original → modded" is mostly the official OTA upgrade; freesleep's own delta is tiny (see Part B).

Eight Sleep uses whimsical codenames throughout: **capybara** (onboarding daemon), **frank/frankenfirmware** (thermal/pump control), **frozen** (sensor+comm MCU subsystem), **burrow/sewer/cage/nest** (provisioning + persistent storage), **defibrillator** (watchdog), **rat-king** (cage/persistent manager), **dac** (device-api-client cloud link).

---

## PART A — Connectivity / Provisioning / Watchdog / UI

### 1. Eight.Capybara  (`opt/eight/bin/Eight.Capybara`, ~77 MB self-contained .NET, upgraded .NET6→.NET9 in OTA)
Unit `capybara.service`: `After=bluetooth NetworkManager cagekeeper`, `Restart=always`, `ExecStopPost=fallback.sh capybara` (5 restarts/120s → fall back). Env sets `DOTNET_BUNDLE_EXTRACT_BASE_DIR=%h/.net`.

Jobs (from `strings`):
- **BLE onboarding** — uses `DotnetBleServer` over BlueZ D-Bus (`org.bluez`): `AdvertisingManager`/`CreateAdvertisement`, `GattServiceBuilder`, `CreateGattService`/`CreateGattCharacteristic`, `ScanCharacteristic` with a pager (`CreateNewPager`) to stream WiFi scan results to the phone app. This is the WiFi-setup GATT server the app talks to during pairing.
- **WiFi scan/connect** — via NetworkManager over D-Bus (`org.freedesktop.NetworkManager.*`): `GetWifiDevices`, `GetAccessPoints` (`AccessPoint.FromNode`), `GetConnectivityState`, `AddAndActivateConnection(2)Async`, `AddConnection`. Device `wlan0`.
- **LED control** — `LedWorkers`, `SelfTestLedUpdate`; LED driver chip identified in strings as **`Is31fl3194`** (ISSI IS31FL3194, 3-channel RGB LED driver over I²C). Supports `BreathingPattern`/`Breathing`, `Color1/2/3`, `ColorBehavior` → this drives the device's status **LED ring** (blue flashing = onboarding/portal mode, referenced in burrow.sh).
- **Self-tests / loopback** — `LoopbackTest`: `TryRunLoopbackTest`, `TestSensorLoopback`, `IsLoopbackActiveAsync`, and the **frozen** tests `TestFrozen`, `TestFrozenComm`.
- **Cloud link (OTA-era, new `opt/eight/config/default.json`)** — `"deviceAPI": "wss://device-api-ws.8slp.net/v1/device"` (System.Net.WebSockets). Same config also makes Capybara the **adjustable-base firmware flasher**: it ships `firmware/8Sleep_H5_SW-V21-2024-10-24.bin` (HW v2/3/4/5) and `8Sleep_H4_SW-V18-2024-07-19.bin` and enforces `expectedVersion`.

**What is "frozen"?** A separate microcontroller subsystem Capybara speaks to over a **local TCP socket** (`frozenPort`, `TcpClient`, `ConnectToTcpHostAsync`, `CopyFrozen`, `FrozenCommands`, `EnsureFrozenForEnqueues`, `_RegisterFrozenSegment`). Capybara runs a **`FrozenRebooter`** with a heartbeat (`CheckHeartbeat`/`CurrentHeartbeat`/`Run`) that reboots/recovers the "frozen" MCU if it stops responding. In Eight's architecture "frozen" is the low-level sensor/comm coprocessor (bed-side sensing + serial/UART bridge) that `frank` (thermal control) and Capybara depend on — hence `frank.service` has `Requires=capybara.service` with the comment "otherwise the self-test breaks uart comms."

### 2. burrow.sh  (provisioning; `opt/eight/bin/burrow.sh`, unit `burrow.service`)
Runs once at first boot, then `systemctl disable burrow.service`. Steps:
- Waits for Capybara to write `/deviceinfo/device-id`; guards on `${SEWER}/burrowing_complete` (`SEWER=/persistent/`).
- Disables `dnsmasq`; temporarily stops `swupdate`.
- **Partition/persistence layout (SEWER):** A/B rootfs on mmc partitions 1 & 2; **partition 3 is the persistent data partition mounted at `/cage`** (`CAGE_DEV=/dev/…p3`). It creates `/cage/<current-root>/`, backs up the previous root's state to `.old`, copies frank state (`settings, heat, alarm.cbr`) and `subsystem_updates` from the other root, and **bind-mounts `/cage/<current-root>` → `/persistent`** (added to `/etc/fstab` as `nofail,bind`). So `/persistent` (a.k.a. SEWER/NEST) is per-slot persistent state.
- **Device identity:** reads `/deviceinfo/device-id` and `/deviceinfo/wifi-macaddress` (written by Capybara from EEPROM), writes `/etc/device_id`, and personalizes `/etc/swupdate.cfg` (DeviceId, VarisciteId=HW serial from `/sys/firmware/devicetree/base/serial-number`, MacAddress, CurrentRev=RAT_VERSION, logurl).
- **Legacy provisions:** if `/cage/provisions.tgz` exists, unpacks to `/persistent/provisions/` (may contain `wg0.conf`).
- **WireGuard:** if `/persistent/provisions/wg0.conf` exists, copies it to `/etc/wireguard/` ("generic image, no vpn" otherwise).
- **WiFi:** installs `/cage/customer-wifi.nmconnection` into NetworkManager; installs `/cage/ssh_host*` keys if present.
- (Large commented-out block shows the **legacy design ran the cloud client as a Docker container** `eightsleep/device-api-client` — since replaced by the native `dac` node service.)
- No base64 blobs present in this copy (the "long lines" are just the sed-personalization lines); no embedded keys/certs in the script itself.
- On completion: `touch /persistent/burrowing_complete`, re-enables `swupdate`, restarts capybara.

`/deviceinfo` is a tmpfs-like dir Capybara populates from the on-board EEPROM (device-id, wifi-macaddress, `dac.sock`). In OTA it lives under `/persistent/deviceinfo/`.

### 3. defibrillator.sh  (watchdog; `defibrillator.service`, `Type=notify`, `WatchdogSec=300s`)
Two jobs: (a) mark a freshly-installed OTA as good, (b) connectivity watchdog.
- **ustate state machine** (u-boot env via `fw_printenv`/`fw_setenv`): `CANTREAD=-1, OK=0, INSTALLED=1, TESTING=2, FAILED=3`. On boot: if `ustate==INSTALLED` → set `TESTING`; if device reaches network → `evaluate_update` sets `OK` (`fw_setenv ustate 0 bootcount 0`). `set_ustate FAILED` flips the boot partition: `fw_setenv ustate 3 mmcpart $OTHER_PART falling_back 1`. Also `fw_setenv upgrade_available 0` each good boot to reset u-boot's altboot counter.
- **Heartbeat loop:** pings `1.1.1.1` (Cloudflare), `8.8.8.8` (Google), or VPN `100.64.0.1`. If WiFi configured but offline for `TIME_TO_BRAIN_DEATH = 5 min` → `reboot_to_fallback` (reboots; falls back partition unless ustate already OK). If no WiFi creds, it will *not* reboot (avoids boot-loop on fresh/factory device) but still marks updates good.
- **VPN self-heal:** if `/etc/wireguard/wg0.conf` present but `wg0` not pingable → `wg-quick down wg0 && wg-quick up wg0`.
- **`size_queen`:** if free space on `/` < 250 MB → `fall_back` (revert partition + reboot).
- **`time_machine`/`ensure_date_sanity`:** forces clock to sane value (≥ 2022-03-02) so TLS certs validate on RTC-less boots.
- `fallback.sh` is the `ExecStopPost` for the critical units; when a unit hits its `StartLimitBurst`, `SERVICE_RESULT=start-limit-hit` triggers `fall_back` (mark FAILED, dump last 250 journal lines, reboot).

### 4. cagekeeper.sh + cage  — **headless, LED ring only (no interactive display)**
Despite the `Documentation=…/rat-king` and "Wayland kiosk" framing, in this image **there is no Wayland compositor** (`cage`/`weston` binaries are absent; the only `cage` present is the `/cage` *data partition* directory). `cagekeeper.sh` simply **fsck's and mounts the `/cage` persistent partition** (partition 3), reformats it on unrecoverable fsck, reboots on fsck code 2, GC's files >2 MB when under 50 MB free, then mounts `/persistent`. Only visible UI is the **LED ring** driven by Capybara (IS31FL3194) plus a boot-time `psplash` framebuffer splash (`psplash-start.service`). OS name "FSLC **FrameBuffer**" confirms no desktop stack. In OTA, `cagekeeper` is **replaced by `persistent-manager.sh`/`persistent-manager.service`** doing the same fsck/mount against a dedicated `persistent.mount`.

### 5. Networking
- **NetworkManager** manages `wlan0` (station) and Capybara drives it over D-Bus. OTA adds `/etc/NetworkManager/NetworkManager.conf` storing keyfiles under **`/persistent/system-connections/`** (WiFi creds survive OTA), and marks `p2p-dev-wlan0` unmanaged.
- **dnsmasq** present but **explicitly disabled at boot** by burrow.sh (was the softAP DHCP server for portal mode).
- **hostapd** (`/etc/hostapd.conf`, `ssid=test`) present for AP/portal onboarding mode.
- **iptables/ip6tables** services enabled but the rules files (`/etc/iptables/iptables.rules`) are **empty** — no host firewall in effect.
- **WireGuard** `wg0` via `wg-quick@.service` (+ `wg-quick.target`); config is provisioned per-device into `/etc/wireguard/wg0.conf` (from `/persistent/provisions/`). VPN subnet `100.64.0.0/10` (CGNAT), server/gateway `100.64.0.1`. Generic images ship without it.
- **Time:** systemd-timesyncd (OTA adds `timesyncd.conf.d`); fallback ntpd logic in globals.sh.

**Every remote host / endpoint the device contacts:**
| Host / target | Purpose | Proto/Port | Notes |
|---|---|---|---|
| `update-api.8slp.net` | OTA updates (SWUpdate suricatta + gservice) & progress logging | HTTPS 443 | `/v1/updates/p1/1`, `/v1/progress/<device-id>`; `nocheckcert=true`; poll 3600s (orig) → **360s (OTA)** |
| `device-api.8slp.net` | DAC (device-api-client) telemetry/control | **CoAP-over-DTLS, UDP 5684** | mutual-TLS with per-device cert/key (`sewerPath=/deviceinfo/`) |
| `device-api-ws.8slp.net` | Capybara cloud control channel (**new in OTA**) | **WSS 443** `/v1/device` | from `opt/eight/config/default.json` |
| `nrl.8slp.net:5044` | journalbeat → Logstash log shipping (**original only**) | TCP 5044 | removed in OTA |
| AWS Kinesis `production-eight-os-logs` (us-east-1) | Vector log sink (**OTA**, replaces journalbeat) | HTTPS | `aws_kinesis_streams`, disk-buffered |
| `100.64.0.1` (WireGuard `wg0`) | management VPN | UDP/WG | per-device provisioned |
| `1.1.1.1`, `8.8.8.8` | connectivity ping checks (defibrillator) | ICMP | not data endpoints |

### 6. Custom systemd units (one line each)
- `capybara.service` — BLE onboarding + WiFi + LED + self-tests + cloud WS/base-firmware (main app daemon).
- `dac.service` — Node "device-api-client": `node build/main.js`, user `dac`, `NODE_ENV=pod3`, CoAP/DTLS link to device-api.8slp.net.
- `burrow.service` — one-shot first-boot provisioning (identity, partitions, VPN, WiFi); self-disables.
- `defibrillator.service` — connectivity watchdog + OTA ustate validator + A/B fallback.
- `cagekeeper.service` — (orig) fsck/mount the `/cage` persistent partition. **Replaced in OTA by `persistent-manager.service`.**
- `frank.service` — waits for provisioning + device-id, then `exec frankenfirmware` (thermal/pump/alarm control) with `DAC_SOCKET=/deviceinfo/dac.sock`; `Requires=capybara`.
- `swupdate.service` (+ `swupdate.socket`, `swupdate-progress`, `swupdate-usb@`) — SWUpdate daemon, gated on `ustate==OK`; enabled by default in OTA.
- `journalbeat.service` — (orig) Elastic Beats log shipper to nrl.8slp.net:5044. **Removed in OTA.**
- `vector.service` (+ `eight-kernel.service`) — (**OTA**) Vector log pipeline: journald → parse Serilog JSON → tag deviceId/version → AWS Kinesis; `eight-kernel.sh` is literally `dmesg -W` piped into vector.
- `persistent-manager.service` — (**OTA**) fsck/format/mount the persistent partition (rat-king).
- `usbnet.service` — (**OTA**) USB-gadget network interface (`/lib/systemd/usbgadget-net.sh`), + `etc/systemd/network/usb.network`.
- `earlyoom.service` — (**OTA**) hardened early-OOM killer (heavy sandboxing, 50 MB cap).
- `variscite-wifi.service` / `variscite-bt.service` — SoM vendor WiFi/BT bring-up.
- `hostapd.service`, `dnsmasq.service` — softAP + DHCP for portal onboarding (dnsmasq disabled at runtime).
- `wg-quick@.service` / `wg-quick.target` — WireGuard management VPN.
- `iptables.service` / `ip6tables.service` — restore firewall rules (rules empty here).
- Vendor/stock: `variscite-*`, `psplash-*`, `rngd`, standard systemd/NetworkManager/bluetooth/wpa_supplicant units.

---

## PART B — Three-Way Diff (original vs modded/freesleep vs ota)

### B0. Overall shape
`diff -rq rootfs-modded rootfs-ota` (whole tree, minus `/lib/modules`) returns **exactly 5 differences** — that is the *entire* freesleep delta. Everything else that differs between original and modded is simply the official OTA upgrade.

### B1. Freesleep's actual changes (modded vs its OTA base) — all it does is add SSH root access
1. **`lib/systemd/system/sshd.service`** (NEW) — a plain always-on OpenSSH server:
   ```
   [Service] ExecStart=/usr/sbin/sshd -D  Restart=always  [Install] WantedBy=multi-user.target
   ```
   (Stock uses socket-activated `sshd.socket` + `sshd@.service`; freesleep replaces that with a persistent daemon so it's always reachable.)
2. **`etc/systemd/system/multi-user.target.wants/sshd.service`** (NEW symlink) — enables the above at boot.
3. **`etc/ssh/authorized_keys`** (NEW) — injects **4 ed25519 public keys** (freesleep maintainer/community keys):
   `AAAA…HSkKiRUUmnErOKGx81nyge/9Kqjk…`, `…FeTK1iARlNIKP/DS8/ObBm9yUM…`, `…KXc9PX3uTYVrgvKdztk+LBh5WMN…`, `…KPnLt84bKhUgFxjQf10+Htro9Lo…`.
4. **`etc/shadow`** (MODIFIED) — sets a **known password** on both `root` and `rewt` (same SHA-512 hash `$6$TuDO46rILr$gkPUuLKZe3pse…`). Stock OTA had `root:*` (locked) and `rewt` on a weak `$1$` MD5 hash. Freesleep unlocks root and standardizes the password.
5. **`etc/NetworkManager/system-connections/EyePhone.nmconnection`** (NEW) — a pre-seeded WiFi client profile (SSID `EyePhone`, WPA-PSK `<redacted>`) so the Pod auto-joins a known phone hotspot for out-of-box SSH access.

**Mechanism / architecture:** Freesleep (at the rootfs-image level) is **purely a root-shell enabler** on the *newer official firmware*. It does **NOT** disable capybara, dac, swupdate/OTA, wireguard, or vector in the image; it does not add a control daemon, web server, or API to the rootfs; `frankenfirmware`/`frank` are untouched. No freesleep app payload exists anywhere in the image (`rg -i freesleep` over `home`/`opt`/`etc` = 0 hits; `/persistent` and `/home/{dac,root,rewt}` are empty in the extracted image). Conclusion: **all freesleep runtime behavior (disabling cloud/OTA, custom scheduling/UI, local API) is applied post-boot over SSH or dropped into `/persistent`, not baked into the flashed rootfs.** The image is the delivery vehicle for the SSH backdoor only.

### B2. What the official OTA upgrade changed (original → ota) — the large diff
Build flavor changed `fslc-framebuffer` → **`fslc-framebuffer-eight`**; dropped Yocto layers meta-qt5, meta-elastic-beats, meta-virtualization, meta-sca (no more Docker/Qt/Beats).
- **Logging re-platformed:** removed `journalbeat` (+ its `nrl.8slp.net:5044` Logstash sink) → added **Vector** (`vector`, `vector.sh`, `vector.toml`, `vector.service`, `eight-kernel.service`) shipping journald to **AWS Kinesis `production-eight-os-logs` (us-east-1)**.
- **Persistence rework:** `cagekeeper.*` → **`persistent-manager.*`** with a dedicated `persistent.mount`; `deviceinfo` moved under `/persistent/deviceinfo/`.
- **New services:** `earlyoom.service` (memory pressure protection), `usbnet.service` + `usb.network` (USB-gadget net), `reset-capybara-cache.sh`, `sysctl.d/panic-reboot.conf` (kernel-panic → reboot), `/etc/default/{earlyoom,watchdog}`, `tmpfiles.d/{dac,dnf}.conf`, `timesyncd.conf.d`.
- **Capybara upgraded** .NET6 → **.NET9**; gains the `wss://device-api-ws.8slp.net/v1/device` WebSocket channel and **adjustable-base firmware flashing** (bundled H4 V18 / H5 V21 `.bin` blobs).
- **OTA cadence:** suricatta `polldelay` **3600s → 360s** (checks 10× more often); swupdate enabled by default (`swupdate.service` + `swupdate.socket` in `multi-user.target.wants`).
- **Hardening that can break community mods:**
  - `sshd_config`: `#PermitRootLogin prohibit-password` → **`PermitRootLogin no`**, and a login `Banner /etc/issue`. → This is precisely why freesleep must ship its **own** `sshd.service` and unlock root in `/etc/shadow` rather than relying on stock SSH.
  - WiFi creds relocated to `/persistent/system-connections/` (survive OTA — but an OTA also re-asserts the stock `/etc/shadow`, so a future official OTA would **wipe freesleep's root password + authorized_keys** in `/etc`, and faster 360s polling means the device pulls official updates aggressively unless OTA is disabled at runtime).
  - `rewt` password hash rotated (`$1$/Vq0KUHB…` → `$1$Sbwfl4Yp…`).
- `NetworkManager.conf` added (keyfile plugin, persistent path). `sudoers.d/001_rewt` renamed `50-rewt` (content identical: `rewt ALL=(ALL) ALL`).

### B3. Net take for a FOSS replacement
- The device is **headless**: only feedback surface is the IS31FL3194 **LED ring** (I²C) + psplash; no display stack to replace.
- Local control plane to reimplement: Capybara's roles (BlueZ GATT WiFi onboarding, NM WiFi mgmt, LED ring, "frozen" MCU TCP link + heartbeat/reboot, adjustable-base flashing) and `frankenfirmware`'s thermal/pump/alarm control over `/deviceinfo/dac.sock` and the "frozen" serial/UART bridge.
- Cloud dependencies to sever for FOSS/offline: `update-api.8slp.net` (OTA — disable `swupdate`), `device-api.8slp.net:5684` (DAC/CoAP) and `device-api-ws.8slp.net` (Capybara WS), Vector→AWS Kinesis, and the WireGuard management VPN.
- The A/B partition + u-boot `ustate` fallback machine (defibrillator) must be respected or neutralized, or a bad image auto-reverts after 5 min offline / on low disk.
- Freesleep itself gives the template: get on the newer firmware, own root via SSH, then disable the cloud/OTA daemons at runtime.
