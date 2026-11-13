{
  config,
  lib,
  pkgs,
  utils,
  ...
}:

let
  cfg = config.programs.genkan;
  # Must match the GeoClue DesktopId used by src/geoclue.rs.
  wallpaperDesktopId = "genkan-wallpaper";
  greeter = cfg.greeter;
  source = cfg.wallpaper.source;
  stateDirectory = "/var/lib/genkan-wallpapers";
  prepareArgs = [
    "assets"
    "prepare"
    "--max-bytes"
    (toString cfg.wallpaper.maxBytes)
  ]
  ++ lib.optional (source.catalog != null) source.catalog
  ++ lib.optionals (source.url != null) [
    "--url"
    source.url
  ]
  ++ lib.optionals (source.file != null) [
    "--file"
    (toString source.file)
  ]
  ++ lib.optionals (source.sha256 != null) [
    "--sha256"
    source.sha256
  ];
  # greetd's PAM session does not inherit the daemon's environment. Both
  # compositor launches and desktop consumers must use this executable.
  launcher = pkgs.writeShellScript "genkan-prepared" ''
    set -eu
    case "''${1-}" in
      login|greeter-session|lock|wallpaper)
        explicit=false
        for arg in "$@"; do
          case "$arg" in
            --wallpaper-file|--wallpaper-file=*|--file|--file=*) explicit=true ;;
          esac
        done
        if ! "$explicit"; then
          if selected=$(GENKAN_WALLPAPER_DIR=${stateDirectory} ${cfg.package}/bin/genkan assets selected); then
            set -- "$@" --wallpaper-file "$selected"
          else
            echo "Genkan: no prepared wallpaper; check genkan-wallpaper.service. Using the media-free fallback." >&2
          fi
        fi
        ;;
    esac
    exec ${cfg.package}/bin/genkan "$@"
  '';
  runtimePackage =
    if source == null then
      cfg.package
    else
      pkgs.symlinkJoin {
        name = "genkan-prepared";
        paths = [ cfg.package ];
        postBuild = ''
          rm "$out/bin/genkan"
          ln -s ${launcher} "$out/bin/genkan"
        '';
      };
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
    ${lib.optionalString (greeter.niri.renderDevice != null) ''
      debug {
          render-drm-device ${quote greeter.niri.renderDevice}
      }
    ''}
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
          "${runtimePackage}/bin/genkan"
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
        renderDevice = lib.mkOption {
          type = lib.types.nullOr (lib.types.strMatching "/dev/dri/[^\"\\]+");
          default = null;
          example = "/dev/dri/by-path/pci-0000:c2:00.0-render";
          description = ''
            DRM render node niri renders with. On hybrid-GPU machines whose
            displays are all wired to one GPU, set this to that GPU's render
            node so the greeter does not render on the other GPU and copy every
            frame across. This replaces wlroots' `WLR_DRM_DEVICES`, which niri
            ignores.
          '';
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

      source = lib.mkOption {
        default = null;
        description = ''
          Optional shared wallpaper provisioning. Choose exactly one catalog ID,
          direct HTTP(S) URL, or local regular file. Media is copied, validated
          and selected by an unprivileged system service, never during login or
          lock. Desktop users can read but cannot modify the shared selection.
        '';
        type = lib.types.nullOr (
          lib.types.submodule {
            options = {
              catalog = lib.mkOption {
                type = lib.types.nullOr lib.types.str;
                default = null;
                example = "iceland";
                description = "ID from genkan assets list.";
              };
              url = lib.mkOption {
                type = lib.types.nullOr (lib.types.strMatching "https?://[^[:space:]]+");
                default = null;
                description = "Direct media URL; redirects are not followed.";
              };
              file = lib.mkOption {
                type = lib.types.nullOr lib.types.path;
                default = null;
                description = "Local media readable by the genkan-wallpapers system user.";
              };
              sha256 = lib.mkOption {
                type = lib.types.nullOr (lib.types.strMatching "[0-9a-fA-F]{64}");
                default = null;
                description = "Optional trusted hexadecimal SHA-256; catalog authenticity pins remain mandatory.";
              };
            };
          }
        );
      };
      maxBytes = lib.mkOption {
        type = lib.types.ints.between 4096 (16 * 1024 * 1024 * 1024);
        default = 2 * 1024 * 1024 * 1024;
        description = "Maximum source and expanded media size for provisioning.";
      };
      package = lib.mkOption {
        type = lib.types.package;
        readOnly = true;
        default = runtimePackage;
        description = "Playback package installed system-wide, also usable by explicit desktop/lock launch commands. Adds the last prepared selection unless --file or --wallpaper-file is supplied.";
      };

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
      environment.systemPackages = [ runtimePackage ];
      security.pam.services.genkan-lock = { };
    })

    (lib.mkIf (source != null) {
      assertions = [
        {
          assertion =
            cfg.enable
            &&
              builtins.length (
                builtins.filter (x: x != null) [
                  source.catalog
                  source.url
                  source.file
                ]
              ) == 1;
          message = "programs.genkan.wallpaper.source requires programs.genkan.enable and exactly one of catalog, url or file.";
        }
      ];
      users.groups.genkan-wallpapers = { };
      users.users.genkan-wallpapers = {
        isSystemUser = true;
        group = "genkan-wallpapers";
      };
      systemd.services.genkan-wallpaper = {
        description = "Prepare and validate Genkan wallpaper";
        wantedBy = [ "multi-user.target" ];
        wants = [ "network-online.target" ];
        after = [ "network-online.target" ];
        before = [ "greetd.service" ];
        environment.GENKAN_WALLPAPER_DIR = stateDirectory;
        serviceConfig = {
          Type = "oneshot";
          RemainAfterExit = true;
          User = "genkan-wallpapers";
          Group = "genkan-wallpapers";
          StateDirectory = "genkan-wallpapers";
          StateDirectoryMode = "0755";
          UMask = "0022";
          ExecStart = utils.escapeSystemdExecArgs ([ "${cfg.package}/bin/genkan" ] ++ prepareArgs);
          TimeoutStartSec = 750;
          NoNewPrivileges = true;
          ProtectSystem = "strict";
          ProtectHome = "read-only";
          PrivateTmp = true;
          PrivateDevices = true;
          RestrictSUIDSGID = true;
        };
      };
      # A failed refresh must not prevent login using the previous selection.
      systemd.services.greetd.wants = lib.mkIf greeter.enable [ "genkan-wallpaper.service" ];
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
                "${runtimePackage}/bin/genkan"
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
