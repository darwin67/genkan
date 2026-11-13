{ pkgs, moduleSystem }:
let
  lib = pkgs.lib;
  raw = pkgs.writeShellScriptBin "genkan" ''
    if [ "''${1-}" = assets ] && [ "''${2-}" = selected ]; then
      test "$GENKAN_WALLPAPER_DIR" = /var/lib/genkan-wallpapers
      test "''${GENKAN_TEST_NO_SELECTION-0}" = 0 || exit 1
      printf '%s\n' '/var/lib/genkan-wallpapers/objects/example with spaces/prepared.mov'
    else
      printf '%s\n' "$@"
    fi
  '';
  system = moduleSystem.extendModules {
    modules = [
      {
        programs.genkan.package = lib.mkForce raw;
        programs.genkan.greeter.enable = true;
        programs.genkan.wallpaper.source.catalog = "iceland";
      }
    ];
  };
  cfg = system.config;
  service = cfg.systemd.services.genkan-wallpaper;
  command = "${cfg.programs.genkan.wallpaper.package}/bin/genkan";
  withSource =
    source:
    (system.extendModules {
      modules = [ { programs.genkan.wallpaper.source = lib.mkForce source; } ];
    }).config;
  urlConfig = withSource { url = "https://example.org/custom.mov"; };
  fileConfig = withSource { file = "/srv/media/local.heic"; };
  encodedUrl = "https://example.org/wall%20paper.mov?literal=$HOME";
  encodedFile = "/srv/media/local %20 $HOME.heic";
  encodedUrlConfig = withSource { url = encodedUrl; };
  encodedFileConfig = withSource { file = encodedFile; };
  encodedUrlUnit =
    pkgs.writeText "encoded-url.service"
      encodedUrlConfig.systemd.units."genkan-wallpaper.service".text;
  encodedFileUnit =
    pkgs.writeText "encoded-file.service"
      encodedFileConfig.systemd.units."genkan-wallpaper.service".text;
  invalid = withSource {
    catalog = "iceland";
    url = "https://example.org/custom.mov";
  };
  niri =
    (system.extendModules {
      modules = [ { programs.genkan.greeter.compositor = "niri"; } ];
    }).config;
in
assert service.serviceConfig.User == "genkan-wallpapers";
assert service.serviceConfig.StateDirectoryMode == "0755";
assert service.serviceConfig.UMask == "0022";
assert service.serviceConfig.NoNewPrivileges;
assert builtins.elem "greetd.service" service.before;
assert builtins.elem "genkan-wallpaper.service" cfg.systemd.services.greetd.wants;
assert !(builtins.elem "genkan-wallpaper.service" cfg.systemd.services.greetd.requires);
assert lib.hasInfix "iceland" service.serviceConfig.ExecStart;
assert lib.hasInfix "--url" urlConfig.systemd.services.genkan-wallpaper.serviceConfig.ExecStart;
assert lib.hasInfix "--file" fileConfig.systemd.services.genkan-wallpaper.serviceConfig.ExecStart;
# The unit must preserve literal percent and dollar characters, not interpret
# them as systemd specifiers or environment substitutions.
assert
  encodedUrlConfig.systemd.services.genkan-wallpaper.serviceConfig.ExecStart
  == ''"${raw}/bin/genkan" "assets" "prepare" "--max-bytes" "2147483648" "--url" "https://example.org/wall%%20paper.mov?literal=$$HOME"'';
assert
  encodedFileConfig.systemd.services.genkan-wallpaper.serviceConfig.ExecStart
  == ''"${raw}/bin/genkan" "assets" "prepare" "--max-bytes" "2147483648" "--file" "/srv/media/local %%20 $$HOME.heic"'';
assert builtins.any (a: !a.assertion && lib.hasInfix "exactly one" a.message) invalid.assertions;
assert lib.hasInfix (builtins.unsafeDiscardStringContext command)
  cfg.services.greetd.settings.default_session.command;
assert builtins.elem cfg.programs.genkan.wallpaper.package cfg.environment.systemPackages;
pkgs.runCommand "genkan-wallpaper-provisioning-check" { } ''
  # Parse the complete generated units with systemd, not a shell. Supply only
  # inert dependency targets so verification does not depend on the build host.
  export SYSTEMD_UNIT_PATH="$PWD"
  export XDG_RUNTIME_DIR="$PWD/runtime"
  mkdir "$XDG_RUNTIME_DIR"
  for target in sysinit basic shutdown network-online; do
    printf '[Unit]\nDescription=Test dependency\nDefaultDependencies=no\n' > "$target.target"
  done
  ${pkgs.systemd}/bin/systemd-analyze --user verify --man=no ${encodedUrlUnit} ${encodedFileUnit}

  command=${command}
  selected='/var/lib/genkan-wallpapers/objects/example with spaces/prepared.mov'
  for mode in login greeter-session lock wallpaper; do
    diff -u <(printf '%s\n' "$mode" --reduce-motion --wallpaper-file "$selected") \
      <("$command" "$mode" --reduce-motion)
    diff -u <(printf '%s\n' "$mode" --wallpaper-file=/explicit.mov) \
      <("$command" "$mode" --wallpaper-file=/explicit.mov)
    diff -u <(printf '%s\n' "$mode") \
      <(GENKAN_TEST_NO_SELECTION=1 "$command" "$mode" 2>error)
    grep -q 'no prepared wallpaper' error
  done
  diff -u <(printf '%s\n' wallpaper --file /explicit.heic --solar) \
    <("$command" wallpaper --file /explicit.heic --solar)
  diff -u <(printf '%s\n' assets list) <("$command" assets list)
  grep -Fq '${niri.programs.genkan.wallpaper.package}/bin/genkan' \
    ${niri.programs.genkan.greeter.niri.configFile}
  touch "$out"
''
