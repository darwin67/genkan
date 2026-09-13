{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.programs.genkan;
  # Must match the GeoClue DesktopId used by src/geoclue.rs.
  wallpaperDesktopId = "genkan-wallpaper";
in
{
  options.programs.genkan = {
    enable = lib.mkEnableOption "Genkan package and session-lock PAM policy";
    package = lib.mkOption {
      type = lib.types.package;
      description = "Genkan package to install.";
    };

    wallpaper = {
      enable = lib.mkEnableOption "Genkan desktop wallpaper runtime";

      solar.enable = lib.mkEnableOption ''
        GeoClue-backed optional solar scheduling for the Genkan desktop
        wallpaper. Enabling this provisions GeoClue and authorizes the
        Genkan wallpaper application identity; it does not start a
        compositor or activate a user session. Solar selection remains
        opt-in at runtime through `genkan wallpaper --solar`.
      '';
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.enable {
      # Idle policy is deliberately host-owned. Enabling this module must not
      # replace a desktop environment's locker or install an automatic hook.
      environment.systemPackages = [ cfg.package ];
      security.pam.services.genkan-lock = { };
    })

    (lib.mkIf (cfg.enable && cfg.wallpaper.solar.enable) {
      assertions = [
        {
          assertion = cfg.wallpaper.enable;
          message = "programs.genkan.wallpaper.solar requires programs.genkan.wallpaper.enable.";
        }
      ];

      # Provision only the high-level GeoClue service and the exact
      # application identity. Raw services.geoclue2 settings remain host
      # configuration, the host-wide agent whitelist is untouched, and
      # isSystem stays false so location access is never a system bypass.
      services.geoclue2 = {
        enable = true;
        appConfig.${wallpaperDesktopId} = {
          isAllowed = true;
          isSystem = false;
        };
      };
    })
  ];
}
