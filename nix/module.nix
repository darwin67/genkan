{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.genkan;
in
{
  options.programs.genkan = {
    enable = lib.mkEnableOption "Genkan login and session-lock frontend";
    package = lib.mkOption {
      type = lib.types.package;
      description = "Genkan package to install.";
    };
  };

  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ];
    security.pam.services.genkan-lock = { };
  };
}
