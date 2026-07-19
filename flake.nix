{
  description = "podd — open firmware for the Eight Sleep Pod";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in
    {
      # Host-side release tooling (podup) + the on-device `podd` daemon and the
      # `pod-probe` diagnostic, cross-compiled to aarch64 for the Pod.
      packages = forAll (pkgs:
        let
          # Static aarch64 (musl): the on-device deps (tokio-serial/serialport,
          # linux-embedded-hal, jiff, rumqttc→rustls→ring) all build against
          # musl, so we get a fully static ELF that runs regardless of the Pod's
          # glibc (Yocto hardknott ships glibc ~2.33). `pkgsStatic` + crt-static
          # gives a binary with no interpreter / no shared-lib deps.
          crossMusl = pkgs.pkgsCross.aarch64-multiplatform-musl.pkgsStatic;

          # aarch64 static musl build of one workspace member.
          mkAarch64 = { pname, member }: crossMusl.rustPlatform.buildRustPackage {
            inherit pname;
            version = "0.0.1";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "-p" member ];
            # Force a fully static binary (no ld-musl interpreter).
            RUSTFLAGS = "-C target-feature=+crt-static";
            # Can't run aarch64 tests on an x86_64 builder.
            doCheck = false;
          };
        in
        rec {
          podup = pkgs.rustPlatform.buildRustPackage {
            pname = "podup";
            version = "0.0.1";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
            # Build just the host tooling for now; podd is a stub.
            cargoBuildFlags = [ "-p" "podup" ];
            cargoTestFlags = [ "-p" "pod-update" ];
            nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];
            # podup shells out to mksquashfs at runtime.
            postInstall = ''
              wrapProgram $out/bin/podup --prefix PATH : ${pkgs.squashfsTools}/bin
            '';
          };

          # Cross builds for the Pod (static aarch64 musl).
          probe-aarch64 = mkAarch64 { pname = "pod-probe"; member = "pod-probe"; };
          podd-aarch64 = mkAarch64 { pname = "podd"; member = "podd"; };

          # Vendored free-sleep React SPA (source only — no committed build
          # output). Builds `ui/` reproducibly: `npm ci` offline from the pinned
          # ui/package-lock.json, then `vite build`, installing the static `dist/`
          # bundle to $out. `podd` serves that as its static asset root.
          ui = pkgs.buildNpmPackage {
            pname = "podd-ui";
            version = "0.0.0";
            src = ./ui;
            # Regenerate after any package-lock.json change:
            #   nix build .#ui  → read the "got:" hash from the mismatch error.
            npmDepsHash = "sha256-P6/qFXgOUXoE+Z0y25unNjlxPyCHWiNMVwdgATyhOZU=";
            nodejs = pkgs.nodejs_22;
            # Upstream deps carry stale peer ranges (e.g. @react-spring/web pins
            # react-dom <=18 while the app is on 19); npm resolves these with
            # overrides interactively, so mirror that for the offline `npm ci`.
            npmFlags = [ "--legacy-peer-deps" ];
            npmBuildScript = "build";
            installPhase = ''
              runHook preInstall
              cp -r dist $out
              runHook postInstall
            '';
          };

          # FHS environment for running Buildroot (os/ clean-room image build).
          # Buildroot hardcodes FHS paths — e.g. dependencies.sh requires `file`
          # at exactly /usr/bin/file, and many package rules assume /bin/bash —
          # which a plain `nix shell` can't provide. This wraps the full Buildroot
          # host-tool set into an FHS sandbox. Because Buildroot needs the host
          # tools but the podd/UI cross-build needs nix (absent inside the FHS),
          # do the nix builds OUTSIDE, then run the Buildroot step inside:
          #   nix build .#podd-aarch64 .#ui
          #   nix build .#buildrootEnv
          #   ./result/bin/podd-buildroot-env -c \
          #     'os/scripts/build-image.sh --no-nix \
          #        --podd-bin result-podd/bin/podd --ui-dir result-ui ...'
          # TODO: teach build-image.sh to do this split + re-exec automatically.
          buildrootEnv = pkgs.buildFHSEnv {
            name = "podd-buildroot-env";
            targetPkgs = p: with p; [
              gcc binutils gnumake bash coreutils which file
              gnused gawk gnugrep diffutils findutils patch
              gnutar gzip bzip2 xz zstd cpio unzip rsync wget git
              bc flex bison ncurses openssl python3 perl util-linux gperf
              pkg-config
            ];
            runScript = "bash";
          };

          default = podup;
        });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            gcc
            pkg-config
            squashfsTools
            minisign
            # frontend build (vendored free-sleep app)
            nodejs_22
          ];
        };

        # Shell for manual `cargo build --target aarch64-unknown-linux-*`.
        # Provides the aarch64 cross gcc used as the linker (see
        # .cargo/config.toml). Note: this only supplies the linker/toolchain;
        # the aarch64 std for the target must come from your rust toolchain
        # (rust-toolchain.toml pins `targets = ["aarch64-unknown-linux-gnu"]`).
        # For a reproducible static build prefer `nix build .#probe-aarch64` /
        # `.#podd-aarch64`.
        cross-aarch64 = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            pkgsCross.aarch64-multiplatform.stdenv.cc
            pkgsCross.aarch64-multiplatform-musl.stdenv.cc
          ];
          env = {
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER = "aarch64-unknown-linux-gnu-gcc";
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "aarch64-unknown-linux-musl-gcc";
          };
        };
      });
    };
}
