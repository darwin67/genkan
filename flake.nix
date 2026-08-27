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
          package = rustPlatform.buildRustPackage {
            pname = "genkan";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postInstall = ''
              wrapProgram $out/bin/genkan \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibraries}
            '';
          };

          devShell = pkgs.mkShell {
            packages = [ rustToolchain ];
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;
          };
        };
    in
    {
      packages = forAllSystems (system: {
        default = (systemConfig system).package;
      });
      devShells = forAllSystems (system: {
        default = (systemConfig system).devShell;
      });
    };
}
