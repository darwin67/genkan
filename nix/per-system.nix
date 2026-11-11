{
  self,
  nixpkgs,
  rust-overlay,
  system,
}:

let
  pkgs = import nixpkgs {
    inherit system;
    overlays = [ rust-overlay.overlays.default ];
  };
  packageVersion = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).package.version;
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
  gstreamerPackages = with pkgs.gst_all_1; [
    gstreamer
    gst-plugins-base
    gst-plugins-good
    gst-plugins-bad
    gst-libav
  ];
  runtimeLibraries =
    with pkgs;
    [
      libheif
      libxkbcommon
      vulkan-loader
      wayland
    ]
    ++ gstreamerPackages;
  gstreamerPluginPath = pkgs.lib.makeSearchPath "lib/gstreamer-1.0" (
    map pkgs.lib.getLib gstreamerPackages
  );
  fontConfig = pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; };

  wallpaperManifest = builtins.fromTOML (builtins.readFile ../assets/wallpapers/manifest.toml);
  # Ordinary checks use only authored, repository-delivered fixtures. Remote
  # catalog entries are metadata, never Nix inputs (including in the dev shell).
  repositoryHeicAssets = map (
    asset:
    let
      source = self + "/${asset.source_path}";
    in
    assert builtins.hashFile "sha256" source == asset.sha256;
    asset // { inherit source; }
  ) (builtins.filter (asset: asset.delivery == "repository") wallpaperManifest.dynamic_heic);
  heicAssetCheck =
    assert builtins.length repositoryHeicAssets > 0;
    pkgs.runCommand "genkan-heic-asset-check"
      {
        nativeBuildInputs = [ pkgs.coreutils ];
      }
      ''
        ${pkgs.lib.concatMapStringsSep "\n" (asset: ''
          test "$(stat -c %s ${asset.source})" = "${toString asset.byte_size}"
          test "$(sha256sum ${asset.source} | cut -d' ' -f1)" = "${asset.sha256}"
        '') repositoryHeicAssets}
        touch $out
      '';
  # Explicit test-only cache: JPEG reference images preserve UI baselines and
  # the tiny authored HEVC clip exercises playback without third-party MOVs.
  # Do not attach this directory to the production package or the dev shell.
  testWallpaperDirectory =
    pkgs.runCommand "genkan-test-wallpapers"
      {
        nativeBuildInputs = [ pkgs.coreutils ];
      }
      ''
        mkdir -p $out/mov $out/heic
        # niri-smoke selects the Tahoe catalog ID, but plays authored pixels.
        ln -s ${../tests/fixtures/asset-preflight/hevc.mov} "$out/mov/tahoe-beach.mov"
        ${pkgs.lib.concatMapStringsSep "\n" (
          wallpaper:
          let
            source = ../assets/wallpapers + "/${wallpaper.poster.file}";
          in
          assert builtins.hashFile "sha256" source == wallpaper.poster.sha256;
          ''
            ln -s ${source} "$out/mov/${wallpaper.poster.file}"
          ''
        ) wallpaperManifest.wallpaper}
        ${pkgs.lib.concatMapStringsSep "\n" (asset: ''
          ln -s ${asset.source} "$out/heic/${asset.install_name}"
        '') repositoryHeicAssets}
      '';

  # Built-in account pictures (RFD 7). The SVG sources and their rendered
  # PNGs are both committed and pinned by hash, so the installed pixels are
  # the same whichever nixpkgs a host builds Genkan with (a renderer upgrade
  # must not change them), and the greeter never parses SVG at runtime.
  # `make update-avatars` re-renders the PNGs with this flake's resvg.
  avatarManifest = builtins.fromTOML (builtins.readFile ../assets/avatars/manifest.toml);
  avatars = map (
    avatar:
    let
      source = ../assets/avatars + "/${avatar.name}.svg";
      png = ../assets/avatars + "/${avatar.name}.png";
    in
    assert builtins.hashFile "sha256" source == avatar.svg_sha256;
    assert builtins.hashFile "sha256" png == avatar.png_sha256;
    avatar // { inherit source png; }
  ) avatarManifest.avatar;
  avatarDirectory =
    pkgs.runCommand "genkan-avatars"
      {
        nativeBuildInputs = [ pkgs.coreutils ];
      }
      ''
        mkdir -p $out
        install -m 0444 ${../assets/avatars/manifest.toml} $out/manifest.toml
        ${pkgs.lib.concatMapStringsSep "\n" (avatar: ''
          install -m 0444 ${avatar.source} $out/${avatar.name}.svg
          install -m 0444 ${avatar.png} $out/${avatar.name}.png
        '') avatars}
      '';

  package = rustPlatform.buildRustPackage {
    pname = "genkan";
    version = packageVersion;
    src = self;
    cargoLock.lockFile = ../Cargo.lock;
    nativeBuildInputs = [
      pkgs.addDriverRunpath
      pkgs.makeWrapper
      pkgs.pkg-config
    ];
    nativeCheckInputs = [ pkgs.curl ];
    buildInputs = gstreamerPackages ++ [
      pkgs.libheif
      pkgs.libxkbcommon
      pkgs.pam
    ];
    postInstall = ''
      wallpaperDirectory=$out/share/genkan/wallpapers
      mkdir -p "$wallpaperDirectory"
      install -m 0444 ${../assets/wallpapers/manifest.toml} "$wallpaperDirectory/manifest.toml"
      ln -s ${avatarDirectory} $out/share/genkan/avatars

      wrapProgram $out/bin/genkan \
        --set FONTCONFIG_FILE ${fontConfig} \
        --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.curl ]} \
        --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibraries} \
        --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${gstreamerPluginPath} \
        --suffix VK_ADD_DRIVER_FILES : ${pkgs.addDriverRunpath.driverLink}/share/vulkan/icd.d

      mkdir -p $out/libexec
      install -m 0755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/genkan-lock-auth \
        $out/libexec/genkan-lock-auth
      rm -f $out/bin/genkan-lock-auth
    '';
    postFixup = ''
      addDriverRunpath $out/bin/.genkan-wrapped
    '';
  };

  e2ePackage = rustPlatform.buildRustPackage {
    pname = "genkan-greetd-e2e";
    version = packageVersion;
    src = self;
    cargoLock.lockFile = ../Cargo.lock;
    cargoBuildFlags = [
      "--no-default-features"
      "--features=e2e"
      "--bin=genkan-greetd-e2e"
    ];
    doCheck = false;
  };

  sessionLockTestPackage = rustPlatform.buildRustPackage {
    pname = "genkan-session-lock-test";
    version = packageVersion;
    src = self;
    cargoLock.lockFile = ../Cargo.lock;
    cargoBuildFlags = [
      "--workspace"
      "--features=genkan/lock-test"
    ];
    doCheck = false;
    nativeBuildInputs = [
      pkgs.makeWrapper
      pkgs.pkg-config
    ];
    buildInputs = gstreamerPackages ++ [
      pkgs.libheif
      pkgs.libxkbcommon
      pkgs.pam
    ];
    postInstall = ''
      wrapProgram $out/bin/genkan \
        --set FONTCONFIG_FILE ${fontConfig} \
        --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.curl ]} \
        --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibraries} \
        --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${gstreamerPluginPath}

      mkdir -p $out/libexec
      install -m 0755 target/${pkgs.stdenv.hostPlatform.rust.rustcTarget}/release/genkan-lock-auth \
        $out/libexec/genkan-lock-auth
      rm -f $out/bin/genkan-lock-auth
    '';
  };

  devShell = pkgs.mkShell {
    packages = [
      pkgs.awscli2
      pkgs.curl
      pkgs.git-cliff
      pkgs.jq
      pkgs.jujutsu
      pkgs.libheif
      pkgs.libxkbcommon
      pkgs.pam
      pkgs.pkg-config
      pkgs.python3
      pkgs.util-linux
      rustToolchain
    ]
    ++ gstreamerPackages;
    LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;
    GST_PLUGIN_SYSTEM_PATH_1_0 = gstreamerPluginPath;
    GENKAN_AVATAR_DIR = avatarDirectory;
  };

  hardwareSmoke = pkgs.writeShellApplication {
    name = "genkan-hardware-smoke";
    runtimeInputs = with pkgs; [
      cage
      coreutils
      gnugrep
      jq
      sway
      util-linux
      vulkan-tools
    ];
    text = ''
      export GENKAN_BIN=${package}/bin/genkan
      export FONTCONFIG_FILE=${fontConfig}
      ${builtins.readFile ../scripts/hardware-smoke.sh}
    '';
  };
  previewEvidenceCapture = import ./tests/preview-evidence.nix {
    inherit pkgs testWallpaperDirectory;
    genkan = package;
    checkBaseline = false;
  };
  moduleSystem = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      ./module.nix
      (
        { pkgs, ... }:
        {
          programs.genkan = {
            enable = true;
            package = package;
          };
          # Avoid forcing nixpkgs' removed unversioned alias while evaluating
          # the generated PAM rules; Genkan does not enable Kanidm.
          services.kanidm.package = pkgs.kanidm_1_8;
          system.stateVersion = "26.05";
        }
      )
    ];
  };
  disabledModuleSystem = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      ./module.nix
      (
        { pkgs, ... }:
        {
          programs.genkan.package = package;
          services.kanidm.package = pkgs.kanidm_1_8;
          system.stateVersion = "26.05";
        }
      )
    ];
  };
  solarModuleSystem = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      ./module.nix
      (
        { pkgs, ... }:
        {
          programs.genkan = {
            enable = true;
            package = package;
            wallpaper = {
              enable = true;
              solar.enable = true;
            };
          };
          services.kanidm.package = pkgs.kanidm_1_8;
          system.stateVersion = "26.05";
        }
      )
    ];
  };
  modulePamPolicy = pkgs.writeText "genkan-lock-pam-policy" (
    moduleSystem.config.security.pam.services.genkan-lock.text
  );
  cageGreeterSystem = moduleSystem.extendModules {
    modules = [ { programs.genkan.greeter.enable = true; } ];
  };
  niriGreeterSystem = moduleSystem.extendModules {
    modules = [
      ({ lib, ... }: {
        programs.genkan.package = lib.mkForce (pkgs.writeShellScriptBin "genkan" "exit 1");
        programs.genkan.greeter = {
          enable = true;
          compositor = "niri";
          authenticationOutput = "Dell Inc. DELL U3223QE 42";
          niri.outputs = {
            "Dell Inc. DELL U3223QE 42" = {
              scale = 1.275;
              position = {
                x = 1920;
                y = 0;
              };
            };
            "DP-10" = {
              mode = "1920x1080@144";
              scale = 1;
            };
            "DP-11" = {
              scale = 1.3;
              transform = "90";
            };
            "eDP-1".scale = 1.2;
          };
          niri.renderDevice = "/dev/dri/by-path/pci-0000:c2:00.0-render";
        };
      })
    ];
  };
  # The same greeter without a render device must not emit a debug block.
  noRenderDeviceConfig =
    (niriGreeterSystem.extendModules {
      modules = [ { programs.genkan.greeter.niri.renderDevice = pkgs.lib.mkForce null; } ];
    }).config.programs.genkan.greeter.niri.configFile;
  greeterModuleCheck =
    assert !moduleSystem.config.services.greetd.enable;
    assert cageGreeterSystem.config.programs.genkan.greeter.compositor == "cage";
    assert !cageGreeterSystem.config.systemd.services.greetd.restartIfChanged;
    assert !niriGreeterSystem.config.systemd.services.greetd.restartIfChanged;
    assert pkgs.lib.hasInfix "cage"
      cageGreeterSystem.config.services.greetd.settings.default_session.command;
    pkgs.runCommand "genkan-greeter-module-check" { } ''
      config=${niriGreeterSystem.config.programs.genkan.greeter.niri.configFile}
      grep -Fq 'render-drm-device "/dev/dri/by-path/pci-0000:c2:00.0-render"' "$config"
      ! grep -Fq 'render-drm-device' ${noRenderDeviceConfig}
      cp "$config" "$out"
    '';
  solarAppConfig = solarModuleSystem.config.services.geoclue2.appConfig."genkan-wallpaper";
  solarGeoclueConfig =
    pkgs.writeText "genkan-geoclue-config"
      solarModuleSystem.config.environment.etc."geoclue/geoclue.conf".text;
  moduleCheck =
    assert builtins.elem package moduleSystem.config.environment.systemPackages;
    assert !(builtins.elem package disabledModuleSystem.config.environment.systemPackages);
    assert !(builtins.hasAttr "genkan-lock" disabledModuleSystem.config.security.pam.services);
    assert solarModuleSystem.config.services.geoclue2.enable;
    assert solarAppConfig.isAllowed;
    assert !solarAppConfig.isSystem;
    assert !disabledModuleSystem.config.services.geoclue2.enable;
    assert !moduleSystem.config.services.geoclue2.enable;
    # Provisioning must not rewrite the host-wide agent whitelist.
    assert
      solarModuleSystem.config.services.geoclue2.whitelistedAgents
      == moduleSystem.config.services.geoclue2.whitelistedAgents;
    assert builtins.length moduleSystem.config.services.geoclue2.whitelistedAgents > 0;
    pkgs.runCommand "genkan-module-check" { nativeBuildInputs = [ pkgs.gnugrep ]; } ''
      grep -F 'pam_unix.so' ${modulePamPolicy}
      grep -F 'pam_deny.so' ${modulePamPolicy}
      ! grep -F 'pam_permit.so' ${modulePamPolicy}
      grep -F 'geoclue-demo-agent' ${solarGeoclueConfig}
      awk 'BEGIN { found = 0 } /^\[/ { found = ($0 == "[genkan-wallpaper]") } found' \
        ${solarGeoclueConfig} | grep -F 'allowed=true'
      awk 'BEGIN { found = 0 } /^\[/ { found = ($0 == "[genkan-wallpaper]") } found' \
        ${solarGeoclueConfig} | grep -F 'system=false'
      touch $out
    '';
  # The production decoder verifies every repository fixture through the
  # explicit test cache, including multi-image, tiled and ICC fixtures. Remote
  # decode claims remain catalog records, not automatic network checks.
  heicDecodeExcludedAssets = builtins.filter (
    asset: !(asset.decode_verified or true)
  ) repositoryHeicAssets;
  heicDecodeCheck =
    assert builtins.length repositoryHeicAssets > 0;
    assert builtins.length heicDecodeExcludedAssets == 0;
    pkgs.runCommand "genkan-heic-decode-check"
      {
        nativeBuildInputs = [ pkgs.coreutils ];
      }
      ''
        ${pkgs.lib.concatMapStringsSep "\n" (asset: ''
          ${package}/bin/genkan verify-wallpapers \
            --file ${testWallpaperDirectory}/heic/${asset.install_name} \
            --expect-frames ${toString asset.structure.image_count}
        '') repositoryHeicAssets}
        touch $out
      '';
in
{
  inherit package devShell previewEvidenceCapture;

  hardwareSmokeApp = {
    type = "app";
    program = "${hardwareSmoke}/bin/genkan-hardware-smoke";
  };

  checks = {
    inherit package;
    module = moduleCheck;
    greeter-module = greeterModuleCheck;
    heic-assets = heicAssetCheck;
    avatars = avatarDirectory;
    heic-decode = heicDecodeCheck;
    wallpaper-package = import ./tests/wallpaper-package.nix {
      inherit pkgs wallpaperManifest;
      genkan = package;
    };
    wallpaper-empty-cache = import ./tests/wallpaper-empty-cache.nix {
      inherit pkgs;
      genkan = package;
    };
    graphics-smoke = import ./tests/graphics-smoke.nix {
      inherit pkgs testWallpaperDirectory;
      genkan = package;
    };
    login-output-smoke = import ./tests/login-output-smoke.nix {
      inherit pkgs testWallpaperDirectory;
      genkan = package;
    };
    desktop-wallpaper-smoke = import ./tests/desktop-wallpaper-smoke.nix {
      inherit pkgs;
      genkan = package;
      lockTestGenkan = sessionLockTestPackage;
    };
    preview-evidence = import ./tests/preview-evidence.nix {
      inherit pkgs testWallpaperDirectory;
      genkan = package;
    };
    session-lock-smoke = import ./tests/session-lock-smoke.nix {
      inherit pkgs;
      genkan = sessionLockTestPackage;
      productionGenkan = package;
    };
    niri-smoke = import ./tests/niri-smoke.nix {
      inherit pkgs testWallpaperDirectory;
      genkan = package;
      lockTestGenkan = sessionLockTestPackage;
    };
  }
  // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
    greetd-e2e = pkgs.testers.runNixOSTest (
      import ./tests/greetd.nix {
        genkanE2e = e2ePackage;
        genkan = package;
      }
    );
    niri-greeter-vm = pkgs.testers.runNixOSTest (
      import ./tests/niri-greeter.nix {
        inherit pkgs testWallpaperDirectory;
        genkan = package;
      }
    );
    session-lock-vm = pkgs.testers.runNixOSTest (
      import ./tests/session-lock-vm.nix {
        genkan = sessionLockTestPackage;
        fixture = ../tests/fixtures/dynamic-heic/synthetic-all-properties.heic;
      }
    );
    geoclue-solar-vm = pkgs.testers.runNixOSTest (
      import ./tests/geoclue-solar-vm.nix {
        genkan = package;
        fixture = ../tests/fixtures/dynamic-heic/synthetic-all-properties.heic;
      }
    );
  };
}
