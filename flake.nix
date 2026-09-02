{
  description = "genkan, a graphical greetd frontend";

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
      packageVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
      wallpaperManifest = builtins.fromTOML (builtins.readFile ./assets/wallpapers/manifest.toml);
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      systemConfig =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          rustToolchain = pkgs.rust-bin.stable."1.94.0".default.override {
            extensions = [
              "clippy"
              "rustfmt"
            ];
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          gstreamerPackages = with pkgs.gst_all_1; [
            gstreamer
            gst-plugins-base
            gst-plugins-good
            gst-plugins-bad
            gst-libav
          ];
          runtimeLibraries =
            with pkgs;
            [
              libxkbcommon
              vulkan-loader
              wayland
            ]
            ++ gstreamerPackages;
          gstreamerPluginPath = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" (
            map pkgs.lib.getLib gstreamerPackages
          );
          wallpapers = map (
            wallpaper:
            let
              posterSource = ./assets/wallpapers + "/${wallpaper.poster.file}";
            in
            assert wallpaper.byte_size < wallpaperManifest.delivery.maximum_cacheable_object_bytes;
            assert builtins.hashFile "sha256" posterSource == wallpaper.poster.sha256;
            wallpaper
            // {
              videoSource = pkgs.fetchurl {
                name = wallpaper.install_name;
                url = wallpaper.r2_url;
                hash = wallpaper.nix_hash;
              };
              inherit posterSource;
            }
          ) wallpaperManifest.wallpaper;
          installWallpaper = wallpaper: ''
            ln -s ${wallpaper.videoSource} "$wallpaperDirectory/${wallpaper.install_name}"
            ln -s ${wallpaper.posterSource} "$wallpaperDirectory/${wallpaper.poster.file}"
          '';
          devWallpaperDirectory = pkgs.linkFarm "genkan-wallpapers" (
            map (wallpaper: {
              name = wallpaper.install_name;
              path = wallpaper.videoSource;
            }) wallpapers
          );
        in
        {
          inherit pkgs;

          package = rustPlatform.buildRustPackage {
            pname = "genkan";
            version = packageVersion;
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [
              pkgs.addDriverRunpath
              pkgs.makeWrapper
              pkgs.pkg-config
            ];
            buildInputs = gstreamerPackages;
            postInstall = ''
              wallpaperDirectory=$out/share/genkan/wallpapers
              mkdir -p "$wallpaperDirectory"
              install -m 0444 ${./assets/wallpapers/manifest.toml} "$wallpaperDirectory/manifest.toml"
              ${pkgs.lib.concatMapStringsSep "\n" installWallpaper wallpapers}

              wrapProgram $out/bin/genkan \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibraries} \
                --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${gstreamerPluginPath} \
                --suffix VK_ADD_DRIVER_FILES : ${pkgs.addDriverRunpath.driverLink}/share/vulkan/icd.d
            '';
            postFixup = ''
              addDriverRunpath $out/bin/.genkan-wrapped
            '';
          };

          e2ePackage = rustPlatform.buildRustPackage {
            pname = "genkan-greetd-e2e";
            version = packageVersion;
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "--no-default-features"
              "--features=e2e"
              "--bin=genkan-greetd-e2e"
            ];
            doCheck = false;
          };

          devShell = pkgs.mkShell {
            packages = [
              pkgs.git-cliff
              pkgs.jq
              pkgs.pkg-config
              pkgs.util-linux
              pkgs.awscli2
              rustToolchain
            ]
            ++ gstreamerPackages;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;
            GST_PLUGIN_SYSTEM_PATH_1_0 = gstreamerPluginPath;
            GENKAN_WALLPAPER_DIR = devWallpaperDirectory;
          };
        };
    in
    {
      packages = forAllSystems (system: {
        default = (systemConfig system).package;
      });
      apps = forAllSystems (
        system:
        let
          config = systemConfig system;
          hardwareSmoke = config.pkgs.writeShellApplication {
            name = "genkan-hardware-smoke";
            runtimeInputs = with config.pkgs; [
              cage
              coreutils
              gnugrep
              jq
              sway
              util-linux
              vulkan-tools
            ];
            text = ''
              export GENKAN_BIN=${config.package}/bin/genkan
              export FONTCONFIG_FILE=${
                config.pkgs.makeFontsConf { fontDirectories = [ config.pkgs.dejavu_fonts ]; }
              }
              ${builtins.readFile ./scripts/hardware-smoke.sh}
            '';
          };
        in
        {
          hardware-smoke = {
            type = "app";
            program = "${hardwareSmoke}/bin/genkan-hardware-smoke";
          };
        }
      );
      checks = forAllSystems (
        system:
        let
          config = systemConfig system;
        in
        {
          package = config.package;
          graphics-smoke = import ./nix/tests/graphics-smoke.nix {
            pkgs = config.pkgs;
            genkan = config.package;
          };
          preview-evidence = import ./nix/tests/preview-evidence.nix {
            pkgs = config.pkgs;
            genkan = config.package;
          };
        }
        // nixpkgs.lib.optionalAttrs (system == "x86_64-linux") {
          greetd-e2e = config.pkgs.testers.runNixOSTest (
            import ./nix/tests/greetd.nix { genkanE2e = config.e2ePackage; }
          );
        }
      );
      devShells = forAllSystems (system: {
        default = (systemConfig system).devShell;
      });
    };
}
