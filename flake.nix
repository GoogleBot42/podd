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
