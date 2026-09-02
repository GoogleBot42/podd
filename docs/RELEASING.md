# Releasing podd (maintainer notes)

**Who this is for / what you'll need:** Maintainers who cut podd releases — whether
for the public project, a fork, or your own devices. You'll need push access to the
repo (GitHub and/or a self-hosted Gitea/Forgejo), and optionally an Ed25519 signing
key if you want signed releases. Regular users don't need any of this; see
[INSTALL.md](INSTALL.md) and [UPDATING.md](UPDATING.md).

Related: **[UPDATING.md](UPDATING.md)** (how devices consume what you publish here).

---

## What a release is

A podd release is these files, named exactly how the on-device update agent
resolves them:

```
manifest.json                 # the release manifest (signed if you have a key, else unsigned)
app-<version>.squashfs        # the podd binary + UI + default configs, packed by `podup`
signing.pub                   # the public verifying key — ONLY present when signing
os-<version>.ext4.zst         # the OS slot image (Tier 1)
podd-sd-<tag>.img.gz          # full flashable SD image for fresh installs
podd-recovery-sd-<tag>.img.gz # that image + the eMMC-install payload (RECOVERY.md)
podd-rootfs-<tag>.tar.gz      # the L2 rootfs tarball (+ .sha256), podd-slot-install.sh's
                              # payload
```

Everything from `os-<version>.ext4.zst` down comes from the slower `os-image`
CI job ([below](#the-os-image-lane)); a release without them is app-only and
fully valid. Only the OS slot image is in the manifest — the SD images and the
rootfs tarball are for humans (`dd` / extract; see
[FLASHING.md](FLASHING.md) and [RECOVERY.md](RECOVERY.md)).

The device fetches `manifest.json`, checks each artifact's SHA-256 (always),
checks the signature (if the owner requires one), then activates the app
squashfs; the OS image is applied to the inactive A/B slot only on an explicit
apply (see [UPDATING.md](UPDATING.md)). The filenames are what `pod-update-agent`'s
GitHub and Gitea sources expect, so **don't rename them.**

---

## Cutting a release with CI (the normal path)

The forges split the work: day-to-day development, PRs, and test/build CI run
on **Gitea**; **releases are built and published on the public GitHub mirror**,
which reacts only to a new tag
arriving via the push mirror. `.github/workflows/release.yml` builds the
bundle via `scripts/build-release.sh` and publishes it to the GitHub Release
for the tag — the URL shape `pod-update-agent`'s GitHub source resolves. To
cut a release, tag on Gitea as usual:

```sh
git tag v0.1.0
git push origin v0.1.0    # Gitea; the push mirror forwards the tag to GitHub
```

The mirrored tag triggers the GitHub workflow's `userland-bundle` job, which:

1. Installs Nix and runs `scripts/build-release.sh`.
2. Builds the aarch64 `podd` binary, the web UI, and the host `podup` tool.
3. Packs them (plus `config.pod4.ron` / `config.pod3.ron` and `podd.service`) into
   `app-<version>.squashfs` via `podup release`.
4. Signs the manifest **if** a signing key secret is set (otherwise unsigned).
5. Self-verifies the release exactly as a device would (`podup verify`).
6. Uploads every file in `dist/` to the GitHub release for the tag.

That takes minutes and is the release. The `os-image` job then spends hours
adding the OS artifacts to it ([below](#the-os-image-lane)).

You can also trigger the workflow manually via **workflow_dispatch**: an
explicit `version` input publishes to that tag's release, an empty one is a
build-only smoke test that publishes nothing and leaves the artifacts on the
run instead. The optional `ref` input picks the commit to build.
`scripts/ci-resolve-version.sh` makes that call once for both jobs.

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
   seed) into a repository secret named **`PODD_SIGNING_KEY`** on the **GitHub**
   mirror (releases are built there): *Settings → Secrets and variables →
   Actions → New repository secret.*

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

## The OS image lane

The `os-image` job in `.github/workflows/release.yml` builds one Buildroot tree
and gets four artifacts out of it: `os-<version>.ext4.zst` (the OTA slot image),
`podd-sd-<tag>.img.gz`, `podd-rootfs-<tag>.tar.gz` (+ `.sha256`) and
`podd-recovery-sd-<tag>.img.gz`. It runs `os/scripts/build.sh` and
`scripts/build-recovery-sd.sh`, then re-runs `scripts/build-release.sh` with
`OS_IMAGE` set so `manifest.json` gains the Os component, and uploads
everything with `--clobber` (so a re-run just overwrites).

It is **additive**: the release already exists when it starts, it never gates
the workflow (`continue-on-error`), and if it fails the release simply stays
app-only. That is a valid release — the manifest has no Os component and
devices keep updating the app tier.

**Watch it, though**: `continue-on-error` means the run goes green either way.
When you expect OS artifacts, open the run and check the `os-image` job.

### Runner budget

Everything about this job is shaped by two hard numbers on a GitHub-hosted
runner: a **6 h job limit** and **~21 GB free disk** on 4 vCPU. The Buildroot
tree peaks around 22 GB (`output/` 15 GB, `dl/` 7 GB with the bare git
mirrors), so the job:

- deletes the preinstalled toolchains it doesn't use (dotnet, Android, GHC,
  CodeQL, Swift, …) — about 20 GB back;
- caches Buildroot's `dl/` (~1.2 GB once the git mirrors are pruned — the
  source *tarballs* are what a later run needs, and having them means the 5 GB
  `linux-imx` clone never happens);
- caches ccache and builds through it (`PODD_BR_CCACHE=1`). `output/` itself
  can't be cached — 15 GB against a 10 GB per-repo cache budget — so every run
  is a cold compile and ccache is what keeps that affordable.

**If a hosted runner can't do it** (the job hits its timeout, or runs out of
disk), move it without editing the workflow: on the GitHub mirror, under
*Settings → Secrets and variables → Actions → Variables*, set

- **`OS_IMAGE_RUNNER`** — a self-hosted runner's label, and
- **`OS_IMAGE_TIMEOUT`** — minutes (the default 350 exists only to stop just
  short of the hosted 6 h kill).

What such a host must provide: `nix` with flakes on `PATH` (the workflow's Nix
installer step is hosted-only, so it would fail on e.g. a NixOS runner),
`e2fsprogs`, `zstd`, and ~40 GB of free disk. Nothing else — the build brings
its own toolchain.

### Test-running it

Dispatch the workflow with an empty `version` (optionally a `ref`): nothing is
published, and the artifacts land on the run itself as `podd-os-artifacts`
instead. Budget several hours.

### Building the same thing by hand

```sh
os/scripts/build.sh                              # -> dist/podd-os.ext4.zst + the rest
OS_IMAGE=dist/podd-os.ext4.zst VERSION=v0.1.0 scripts/build-release.sh
```

(`OS_VERSION` defaults to the app version; a raw `rootfs.ext2` input is
zstd-compressed automatically.) From an existing Buildroot tree,
`os/scripts/package-rootfs.sh` re-emits the rootfs tarball in seconds without
rebuilding, and `scripts/build-recovery-sd.sh --plan` prints the recovery-SD
assembly plan and what it deliberately does *not* do.
