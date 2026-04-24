{
  description = "Fono — lightweight native voice dictation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
      in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "fono";
          version = "0.1.0";
          src = ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [
            alsa-lib
            libxkbcommon
            gtk3
            libayatana-appindicator
            xdotool
          ];

          # Single binary only — the rest of the workspace is libraries
          # consumed by the fono bin.
          cargoBuildFlags = [ "-p" "fono" ];

          postInstall = ''
            install -Dm644 packaging/slackbuild/fono/fono.desktop \
              $out/share/applications/fono.desktop
            install -Dm644 packaging/slackbuild/fono/fono.svg \
              $out/share/icons/hicolor/scalable/apps/fono.svg
            install -Dm644 packaging/systemd/fono.service \
              $out/lib/systemd/user/fono.service
          '';

          meta = with pkgs.lib; {
            description = "Lightweight native voice dictation";
            homepage = "https://github.com/NimbleX/fono";
            license = licenses.gpl3Only;
            mainProgram = "fono";
            platforms = platforms.linux ++ platforms.darwin;
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            cargo rustc rustfmt clippy rust-analyzer
            pkg-config alsa-lib gtk3 libayatana-appindicator xdotool
          ];
        };
      });
}
