---
paths:
  - os/**
  - install/**
  - scripts/**
---

# Media writes

- Sanctioned raw-write paths are exactly two: `install/podd-slot-install.sh`
  (below) and pod-update-agent's `AbSlotWriter`
  (`crates/pod-update-agent/src/os_slot.rs`), which writes ONLY the inactive SD
  slot (`/dev/mmcblk1p1`/`p2`), readback-verifies every write, and
  structurally refuses eMMC/mounted targets. Anything else writing raw media
  is a bug.
- eMMC (`mmcblk2` on-device) is never a write target. Safety model:
  SD-swap-to-revert — everything boots from a spare SD, stock card is a
  total, instant revert. `install/podd-slot-install.sh` is the sole
  exception (eMMC A/B slot install) and only ever touches the inactive slot.
- Before `dd`-ing any image, confirm the target is a real block device, not
  a regular file — a misdirected write to a plain file has silently
  absorbed gigabytes of image data with no error.
- Beware SD cards that misreport capacity: unexplained early `ENOSPC` during
  a write is a common symptom of a counterfeit/undersized card, not a script
  bug — see the "verify your SD card" callout in `docs/RECOVERY.md`.
- Always verify a raw write: `cmp -n <byte-count> image.img /dev/sdX`.
- `/var/log` is tmpfs on the image; persistent boot logs are baked in at
  `/data/bootlog` (partition p3). See the `build-sd-image` skill's
  post-mortem section.
- `printf '%s'` does not expand `\t`/`\n` — a literal `\t` in a generated
  `fstab` line once took out the `/data` mount (commit `4cea0fd`). Use
  `printf '%b'`, `echo`, or literal characters instead.
