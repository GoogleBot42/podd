# Releasing podd (maintainer notes)

**Who this is for / what you'll need:** Maintainers who cut podd releases — whether
for the public project, a fork, or your own devices. You'll need push access to the
repo (GitHub and/or a self-hosted Gitea/Forgejo), and optionally an Ed25519 signing
key if you want signed releases. Regular users don't need any of this; see
[INSTALL.md](INSTALL.md) and [UPDATING.md](UPDATING.md).

Related: **[UPDATING.md](UPDATING.md)** (how devices consume what you publish here).

---

## What a release is

A podd release is three files, named exactly how the on-device update agent
resolves them:

```
manifest.json          # the release manifest (signed if you have a key, else unsigned)
app-<version>.squashfs  # the podd binary + UI + default configs, packed by `podup`
signing.pub             # the public verifying key — ONLY present when signing
```

The device fetches `manifest.json`, checks the artifact's SHA-256 (always), checks
the signature (if the owner requires one), then activates the app squashfs. Those
filenames are what `pod-updater`'s GitHub and Gitea sources expect, so **don't
rename them.**

---

## Cutting a release with CI (the normal path)

Both `.github/workflows/release.yml` and `.gitea/workflows/release.yml` build the
**same** bundle via `scripts/build-release.sh` and publish it to the release for
the tag. To cut a release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

Pushing a `v*` tag triggers the workflow, which:

1. Installs Nix and runs `scripts/build-release.sh`.
2. Builds the aarch64 `podd` binary, the web UI, and the host `podup` tool.
3. Packs them (plus `config.pod4.ron` / `config.pod3.ron` and `podd.service`) into
   `app-<version>.squashfs` via `podup release`.
4. Signs the manifest **if** a signing key secret is set (otherwise unsigned).
5. Self-verifies the release exactly as a device would (`podup verify`).
6. Uploads every file in `dist/` to the GitHub / Gitea release for the tag.

You can also trigger it manually via **workflow_dispatch** with an explicit
`version` input.

> The app version drops the leading `v` — tag `v0.1.0` produces
> `app-0.1.0.squashfs`. The manifest carries the full version, and the device reads
> the artifact filename back out of the manifest, so nothing needs to guess.

> The build stamp baked *into* the binary and the UI bundle (the Settings
> device-info chips, `crates/podd-core/build.rs`) is not the tag: `nix build`
> gets no `.git` and no environment from the caller, so `flake.nix` stamps the
> flake revision instead — `0.0.1-g<shortRev>` plus the short commit. That
> identifies the release commit exactly; the tag it was cut as lives in the
> manifest and in `dist/VERSION`.

### Building a release locally

`scripts/build-release.sh` runs the same steps outside CI (needs `nix` with flakes
on PATH):

```sh
VERSION=v0.1.0 CHANNEL=stable ./scripts/build-release.sh
# -> dist/manifest.json, dist/app-0.1.0.squashfs (+ dist/signing.pub if signing)
```

Then upload everything in `dist/` to the release for that tag. Useful env:
`CHANNEL` (default `stable`), `OUT_DIR` (default `dist`), `VARIANTS` (default
`pod4 pod3`).

---

## Setting up CI signing (optional)

Signing is optional — an unsigned release still has its artifact digests enforced.
If you want signed releases so owners can require authenticity:

1. **Generate a keypair** on a trusted, offline machine:

   ```sh
   podup keygen --out-dir keys
   #   keys/signing.key   (SECRET base64 Ed25519 seed — never commit, never share)
   #   keys/signing.pub   (public verifying key — safe to publish)
   ```

2. **Add the secret to CI.** Put the *contents* of `keys/signing.key` (the base64
   seed) into a repository secret named **`PODD_SIGNING_KEY`**:
   - GitHub: *Settings → Secrets and variables → Actions → New repository secret.*
   - Gitea: *Settings → Actions → Secrets.*

3. **(Optional) `PODD_SIGNING_PUB`.** The build can derive `signing.pub` from the
   private seed automatically (via `openssl`), so you usually only need the one
   secret. If you'd rather provide the public key explicitly, set
   `PODD_SIGNING_PUB` (base64 verifying key) as well — or commit `signing.pub` to
   the repo root or `install/`. Resolution order is:
   `PODD_SIGNING_PUB` → committed `signing.pub` → `install/signing.pub` → derived
   from the seed.

When `PODD_SIGNING_KEY` is present the workflow publishes a signed `manifest.json`
plus `signing.pub`; when it's absent the manifest is unsigned (digests still
enforced).

> **Keep `signing.key` offline.** The device only ever *verifies* — the private key
> never needs to touch a Pod. Distribute `signing.pub` to owners so they can set
> `PODD_UPDATER_TRUST` (see [UPDATING.md](UPDATING.md#trust-policy)).

---

## Gitea / Forgejo runner requirements

If you publish to a self-hosted Gitea/Forgejo, the runner needs a bit more than the
GitHub one (which uses the DeterminateSystems Nix action):

- **A runner with Nix, or an image the installer works in.** The canonical
  setup (`runs-on: nixos`, same as `.gitea/workflows/ci.yml`) is a bare NixOS
  host with `nix`, `git`, and `curl` in `/run/current-system/sw/bin`; the
  workflow's Install Nix step no-ops there. On a Docker-image runner instead,
  use an image with `curl`, `git`, and `sudo` (e.g. `catthehacker/ubuntu`) and
  relabel the jobs — the step then installs Nix with the official Determinate
  installer script.
- **The auto-injected `GITHUB_TOKEN`** (Gitea provides `GITHUB_*` for
  compatibility). `scripts/upload-release-gitea.sh` uses it to create/find the
  release and upload assets via the Gitea API — no external actions required.

The uploaded assets are given their bare filenames so the device resolves
`<host>/<owner>/<repo>/releases/download/<tag>/manifest.json` (and the artifact)
exactly as `pod-updater`'s Gitea source expects.

---

## Full-firmware / recovery-SD artifacts (not built yet)

The `recovery-sd` job in both workflows is intentionally **gated off**
(`if: false`). It depends on the L2 podd OS rootfs (`podd-rootfs.tar.gz`) and the
bootable recovery-SD image, which **aren't built yet**. Until those inputs exist,
CI produces only the userland bundle — which is the primary, recommended
deliverable. See `scripts/build-recovery-sd.sh --plan` and the design in
`docs/research/flashing-method.md` §5–§6 for what's still needed.
