{
  description = "podd — open firmware for the Eight Sleep Pod";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (s: f nixpkgs.legacyPackages.${s});
    in
    {
      # Host-side release tooling (podup + pod-update). The on-device `podd`
      # daemon build (with the opensleep control core + aarch64 cross) lands
      # here as that integration proceeds.
      packages = forAll (pkgs: rec {
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
      });
    };
}
