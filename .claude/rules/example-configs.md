---
paths:
  - config.example.ron
  - config.pod3.example.ron
  - config.pod4.example.ron
---

# Example configs

- The vibration alarm block in every example config ships commented out.
  Keep it that way — do not uncomment it, add a new active alarm block, or
  "helpfully" restore it as a default.
- Root cause history: an active example alarm was baked into fresh installs
  and was also copied live onto a config whose owners never wanted an alarm
  by an external migration process (2026-07-20 incident). Commit `9c8697b`
  ("examples: ship with the vibration alarm disabled") is the fix; don't
  regress it.
- These files feed real installs directly: `config.pod4.example.ron` is
  baked into the OS image as the seeded first-boot config
  (`post-build.sh`/`install/` flow), and `config.example.ron` /
  `config.pod3.example.ron` are what installers and hand-authored configs
  copy from. A change here propagates to strangers' beds, not just this repo.
- If you're hand-authoring a config from one of these examples, leave the
  alarm commented out unless the bed's owner explicitly opted in.
