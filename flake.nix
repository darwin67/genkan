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
    { self, nixpkgs, rust-overlay }:
    let
      packageVersion = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
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
          runtimeLibraries = with pkgs; [
            libxkbcommon
            vulkan-loader
            wayland
          ];
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
            ];
            postInstall = ''
              wrapProgram $out/bin/genkan \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibraries} \
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
              rustToolchain
            ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;
          };
        };
    in
    {
      packages = forAllSystems (system: {
        default = (systemConfig system).package;
      });
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
