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
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      perSystem = nixpkgs.lib.genAttrs systems (
        system:
        import ./nix/per-system.nix {
          inherit
            self
            nixpkgs
            rust-overlay
            system
            ;
        }
      );
    in
    {
      packages = nixpkgs.lib.mapAttrs (_: config: {
        default = config.package;
        preview-evidence-capture = config.previewEvidenceCapture;
      }) perSystem;
      apps = nixpkgs.lib.mapAttrs (_: config: {
        hardware-smoke = config.hardwareSmokeApp;
      }) perSystem;
      checks = nixpkgs.lib.mapAttrs (_: config: config.checks) perSystem;
      devShells = nixpkgs.lib.mapAttrs (_: config: { default = config.devShell; }) perSystem;
    };
}
