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
  greeter = cfg.greeter;
  quote = builtins.toJSON;
  outputConfig = lib.concatStringsSep "\n" (
    lib.mapAttrsToList (name: output: ''
      output ${quote name} {
        ${lib.optionalString (!output.enable) "off"}
        ${lib.optionalString (output.mode != null) "mode ${quote output.mode}"}
        ${lib.optionalString (output.scale != null) "scale ${toString output.scale}"}
        transform ${quote output.transform}
        ${lib.optionalString (
          output.position != null
        ) "position x=${toString output.position.x} y=${toString output.position.y}"}
      }
    '') greeter.niri.outputs
  );
  mediaArgs = [
    "--wallpaper"
    greeter.wallpaper
    "--appearance"
    greeter.appearance
  ]
  ++ lib.optionals greeter.reduceMotion [ "--reduce-motion" ]
  ++ lib.optionals (greeter.wallpaperFile != null) [
    "--wallpaper-file"
    (toString greeter.wallpaperFile)
  ]
  ++ lib.optionals (greeter.authenticationOutput != null) [
    "--authentication-output"
    greeter.authenticationOutput
  ];
  niriConfigSource = pkgs.writeText "genkan-greeter-niri.kdl" ''
    ${outputConfig}
    binds {}
    hotkey-overlay { skip-at-startup; }
    screenshot-path null
    xwayland-satellite { off; }
    layout { background-color "transparent"; }
    layer-rule {
      match namespace="^genkan-wallpaper$"
      place-within-backdrop true
    }
    window-rule {
      match app-id="^genkan$"
      ${lib.optionalString (
        greeter.authenticationOutput != null
      ) "open-on-output ${quote greeter.authenticationOutput}"}
      open-fullscreen true
    }
    spawn-at-startup ${
      lib.concatMapStringsSep " " quote (
        [
          "${cfg.package}/bin/genkan"
          "greeter-session"
        ]
        ++ mediaArgs
      )
    }
  '';
  niriConfig = pkgs.runCommand "genkan-validated-greeter.kdl" { } ''
    ${greeter.niri.package}/bin/niri validate --config ${niriConfigSource}
    cp ${niriConfigSource} "$out"
  '';
  niriLauncher = pkgs.writeShellScript "genkan-niri-greeter" ''
    set -eu
    # niri falls back to built-in spawn bindings on startup parse errors.
    # Revalidate with the exact runtime binary before it owns the displays.
    ${greeter.niri.package}/bin/niri validate --config ${niriConfig}
    exec ${greeter.niri.package}/bin/niri --config ${niriConfig}
  '';
in
{
  options.programs.genkan = {
    enable = lib.mkEnableOption "Genkan package and session-lock PAM policy";
    package = lib.mkOption {
      type = lib.types.package;
      description = "Genkan package to install.";
    };

    greeter = {
      enable = lib.mkEnableOption "the Genkan greetd greeter";
      compositor = lib.mkOption {
        type = lib.types.enum [
          "cage"
          "niri"
        ];
        default = "cage";
        description = "Greeter compositor. Cage preserves spanning placement.";
      };
      authenticationOutput = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Preferred connector or niri make/model/serial monitor name.";
      };
      wallpaper = lib.mkOption {
        type = lib.types.enum [
          "tahoe-beach"
          "sequoia-sunrise"
          "sequoia-morning"
          "sequoia-night"
          "golden-gate"
        ];
        default = "tahoe-beach";
        description = "Login wallpaper, also used on the other outputs.";
      };
      wallpaperFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Optional absolute local MOV or HEIC override, readable by greeter.";
      };
      reduceMotion = lib.mkEnableOption "fixed MOV posters and immediate HEIC changes";
      appearance = lib.mkOption {
        type = lib.types.enum [
          "automatic"
          "light"
          "dark"
        ];
        default = "automatic";
        description = "Appearance preference for HEIC wallpapers.";
      };
      niri = {
        package = lib.mkOption {
          type = lib.types.package;
          default = pkgs.niri;
          defaultText = lib.literalExpression "pkgs.niri";
          description = "niri used both to validate and run the greeter configuration.";
        };
        outputs = lib.mkOption {
          default = { };
          description = "Host-owned output blocks. Only output properties are accepted, never arbitrary KDL or includes.";
          type = lib.types.attrsOf (
            lib.types.submodule {
              options = {
                enable = lib.mkOption {
                  type = lib.types.bool;
                  default = true;
                };
                mode = lib.mkOption {
                  type = lib.types.nullOr (lib.types.strMatching "[0-9]+x[0-9]+(@[0-9]+(\\.[0-9]+)?)?");
                  default = null;
                  description = "Resolution and optional refresh rate, e.g. 3840x2160@60.";
                };
                scale = lib.mkOption {
                  type = lib.types.nullOr (
                    lib.types.addCheck (lib.types.either lib.types.int lib.types.float) (n: n > 0)
                  );
                  default = null;
                  description = "Output scale, including fractional values.";
                };
                transform = lib.mkOption {
                  type = lib.types.enum [
                    "normal"
                    "90"
                    "180"
                    "270"
                    "flipped"
                    "flipped-90"
                    "flipped-180"
                    "flipped-270"
                  ];
                  default = "normal";
                };
                position = lib.mkOption {
                  default = null;
                  type = lib.types.nullOr (
                    lib.types.submodule {
                      options = {
                        x = lib.mkOption { type = lib.types.int; };
                        y = lib.mkOption { type = lib.types.int; };
                      };
                    }
                  );
                };
              };
            }
          );
        };
        outputConfig = lib.mkOption {
          type = lib.types.lines;
          readOnly = true;
          default = outputConfig;
          description = "Generated output blocks, reusable in the host's desktop niri configuration.";
        };
        configFile = lib.mkOption {
          type = lib.types.path;
          readOnly = true;
          default = niriConfig;
          description = "Immutable validated greeter configuration (not the desktop configuration).";
        };
      };
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

    (lib.mkIf greeter.enable {
      assertions = [
        {
          assertion = cfg.enable;
          message = "programs.genkan.greeter requires programs.genkan.enable.";
        }
      ];
      services.greetd = {
        enable = true;
        settings.default_session.command =
          if greeter.compositor == "niri" then
            toString niriLauncher
          else
            lib.escapeShellArgs (
              [
                "${pkgs.cage}/bin/cage"
                "--"
                "${cfg.package}/bin/genkan"
                "login"
              ]
              ++ mediaArgs
            );
      };
      services.accounts-daemon.enable = true;
      hardware.graphics.enable = true;
      systemd.services.greetd = {
        restartIfChanged = false;
        environment.XDG_DATA_DIRS = "${config.services.displayManager.sessionData.desktops}/share";
      };
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
