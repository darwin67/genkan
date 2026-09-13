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

    (lib.mkIf cfg.wallpaper.solar.enable {
      assertions = [
        {
          assertion = cfg.enable && cfg.wallpaper.enable;
          message = "programs.genkan.wallpaper.solar requires programs.genkan.enable and programs.genkan.wallpaper.enable.";
        }
      ];
    })

    (lib.mkIf (cfg.enable && cfg.wallpaper.enable && cfg.wallpaper.solar.enable) {
      # Provision only the high-level GeoClue service and the exact
      # application identity. Raw services.geoclue2 settings remain host
      # configuration and the host-wide agent whitelist is untouched.
      #
      # GeoClue treats a non-flatpak client as a system component and may
      # complete Start without consulting the agent, so `isSystem = false`
      # alone is not what grants access. A user-session agent must still be
      # present; enabling the packaged demo agent by default supplies one for
      # compositors that ship no agent of their own. Hosts may override it.
      services.geoclue2 = {
        enable = lib.mkDefault true;
        enableDemoAgent = lib.mkDefault true;
        appConfig.${wallpaperDesktopId} = lib.mkDefault {
          isAllowed = true;
          isSystem = false;
        };
      };
    })
  ];
}
