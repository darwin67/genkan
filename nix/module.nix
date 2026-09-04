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
    enable = lib.mkEnableOption "Genkan package and session-lock PAM policy";
    package = lib.mkOption {
      type = lib.types.package;
      description = "Genkan package to install.";
    };
  };

  config = lib.mkIf cfg.enable {
    # Idle policy is deliberately host-owned. Enabling this module must not
    # replace a desktop environment's locker or install an automatic hook.
    environment.systemPackages = [ cfg.package ];
    security.pam.services.genkan-lock = { };
  };
}
