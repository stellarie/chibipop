{
  description = "Lightweight popup dict for Windows and Wayland on Linux, inspired by the creator of the wei method and weikipop";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      lib = nixpkgs.lib;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      version = (fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

      homeManagerModule =
        {
          config,
          lib,
          pkgs,
          ...
        }:
        let
          cfg = config.programs.chibipop;
        in
        {
          options.programs.chibipop = {
            enable = lib.mkEnableOption "chibipop Japanese lookup";

            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
              defaultText = lib.literalExpression "inputs.chibipop.packages.\${pkgs.stdenv.hostPlatform.system}.default";
              description = "The chibipop package to install.";
            };

            systemd.enable = lib.mkEnableOption "the chibipop systemd user service";
          };

          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];

            systemd.user.services.chibipop = lib.mkIf cfg.systemd.enable {
              Unit = {
                Description = "chibipop Japanese lookup daemon";
                PartOf = [ "graphical-session.target" ];
                After = [ "graphical-session.target" ];
              };
              Service = {
                Type = "simple";
                ExecStart = "${lib.getExe cfg.package} run";
                Restart = "on-failure";
                Slice = "app.slice";
              };
              Install.WantedBy = [ "graphical-session.target" ];
            };
          };
        };

      systemOutputs = lib.genAttrs systems (
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            config.allowUnfree = true;
            overlays = [ rust-overlay.overlays.default ];
          };

          rustToolchain = pkgs.rust-bin.stable.latest.default;
          devToolchain = rustToolchain.override {
            extensions = [ "rust-analyzer" ];
          };

          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };

          mkVariant =
            cudaSupport:
            let
              onnxruntime = pkgs.onnxruntime.override { inherit cudaSupport; };
              libraries = [
                onnxruntime
                pkgs.wayland
                pkgs.libxkbcommon
              ];
              libraryPath = pkgs.lib.makeLibraryPath libraries;
              xkbConfigRoot = "${pkgs.xkeyboard_config}/share/X11/xkb";
            in
            {
              package = rustPlatform.buildRustPackage {
                pname = "chibipop";
                inherit version;

                src = ./.;
                cargoLock.lockFile = ./Cargo.lock;

                cargoBuildFlags = [
                  "-p"
                  "chibipop-linux"
                  "--no-default-features"
                  "--features"
                  "system-onnxruntime"
                ];
                doCheck = false;

                nativeBuildInputs = [ pkgs.makeWrapper ];
                buildInputs = libraries;

                postInstall = ''
                  install -Dm644 data/deconjugator.json \
                    "$out/share/chibipop/data/deconjugator.json"
                  install -Dm644 data/ipadic/system.dic data/ipadic/COPYING \
                    data/ipadic/NOTICE data/ipadic/SHA256SUMS.txt \
                    -t "$out/share/chibipop/data/ipadic"
                  install -Dm644 \
                    crates/chibipop-linux/models/meiki/*.onnx \
                    crates/chibipop-linux/models/meiki/SHA256SUMS.txt \
                    crates/chibipop-linux/models/meiki/LICENSE.md \
                    -t "$out/share/chibipop/models/meiki"

                  install -Dm644 extras/chibipop.desktop \
                    "$out/share/applications/chibipop.desktop"
                  mkdir -p "$out/lib/systemd/user"
                  substitute extras/chibipop.service \
                    "$out/lib/systemd/user/chibipop.service" \
                    --replace-fail /usr/bin/chibipop "$out/bin/chibipop"
                  install -Dm644 crates/chibipop-windows/assets/chibipop.svg \
                    "$out/share/icons/hicolor/scalable/apps/chibipop.svg"

                  install -Dm644 LICENSE \
                    "$out/share/licenses/chibipop/LICENSE"
                  install -Dm644 crates/chibipop-linux/models/meiki/LICENSE.md \
                    "$out/share/licenses/chibipop/LICENSE.models.md"

                  wrapProgram "$out/bin/chibipop" \
                    --prefix LD_LIBRARY_PATH : "${libraryPath}" \
                    --set XKB_CONFIG_ROOT "${xkbConfigRoot}"
                '';

                passthru = { inherit cudaSupport; };

                meta = with lib; {
                  description = "Hover-to-read Japanese lookup for Wayland";
                  homepage = "https://github.com/stellarie/chibipop";
                  license = [
                    licenses.gpl3Plus
                    licenses.lgpl3Only
                  ];
                  mainProgram = "chibipop";
                  platforms = platforms.linux;
                };
              };

              devShell = pkgs.mkShell {
                packages = [
                  devToolchain
                  pkgs.pkg-config
                  pkgs.xkeyboard_config
                ]
                ++ libraries;

                shellHook = ''
                  export LD_LIBRARY_PATH="${libraryPath}:''${LD_LIBRARY_PATH:-}"
                  export XKB_CONFIG_ROOT="${xkbConfigRoot}"
                '';
              };
            };

          cpu = mkVariant false;
          cuda = mkVariant true;
        in
        {
          packages = {
            default = cpu.package;
            chibipop = cpu.package;
            cuda = cuda.package;
          };
          devShells = {
            default = cpu.devShell;
            cuda = cuda.devShell;
          };
        }
      );
    in
    {
      packages = lib.mapAttrs (_: outputs: outputs.packages) systemOutputs;
      devShells = lib.mapAttrs (_: outputs: outputs.devShells) systemOutputs;

      homeManagerModules = {
        chibipop = homeManagerModule;
        default = homeManagerModule;
      };
    };
}
