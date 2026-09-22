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
  wallpapers = map (
    wallpaper:
    let
      posterSource = ../assets/wallpapers + "/${wallpaper.poster.file}";
    in
    assert wallpaper.byte_size < wallpaperManifest.delivery.maximum_cacheable_object_bytes;
    assert builtins.hashFile "sha256" posterSource == wallpaper.poster.sha256;
    wallpaper
    // {
      videoSource = pkgs.fetchurl {
        name = wallpaper.install_name;
        url = wallpaper.r2_url;
        hash = wallpaper.nix_hash;
      };
      inherit posterSource;
    }
  ) wallpaperManifest.wallpaper;
  # MOV wallpapers and their posters are installed under `mov/`, and dynamic
  # HEIC assets under `heic/`, so the installed tree names the format instead of
  # mixing both under one directory. The R2 layout mirrors it.
  installWallpaper = wallpaper: ''
    ln -s ${wallpaper.videoSource} "$wallpaperDirectory/mov/${wallpaper.install_name}"
    ln -s ${wallpaper.posterSource} "$wallpaperDirectory/mov/${wallpaper.poster.file}"
  '';

  # Immutable dynamic HEIC assets. A repository-delivered asset is pinned by its
  # committed bytes; a catalog asset delivered from the R2 host records
  # `delivery = "r2"` with `r2_url` and `nix_hash` and is fetched as a
  # hash-pinned fixed-output source exactly like the MOV catalog.
  dynamicHeicAssets = map (
    asset:
    let
      repositorySource = self + "/${asset.source_path}";
      source =
        if asset.delivery == "r2" then
          pkgs.fetchurl {
            name = asset.install_name;
            url = asset.r2_url;
            hash = asset.nix_hash;
          }
        else
          repositorySource;
    in
    assert asset.delivery == "r2" || asset.delivery == "repository";
    assert asset.delivery != "repository" || builtins.hashFile "sha256" repositorySource == asset.sha256;
    asset
    // {
      inherit source;
    }
  ) wallpaperManifest.dynamic_heic;
  installHeicAsset = asset: ''
    ln -s ${asset.source} "$wallpaperDirectory/heic/${asset.install_name}"
  '';
  heicAssetCheck =
    pkgs.runCommand "genkan-heic-asset-check"
      {
        nativeBuildInputs = [ pkgs.coreutils ];
      }
      ''
        ${pkgs.lib.concatMapStringsSep "\n" (asset: ''
          test "$(stat -c %s ${asset.source})" = "${toString asset.byte_size}"
          test "$(sha256sum ${asset.source} | cut -d' ' -f1)" = "${asset.sha256}"
        '') dynamicHeicAssets}
        touch $out
      '';
  # The development directory mirrors the installed tree: `mov/` for the MOV
  # videos and `heic/` for the dynamic HEICs. `linkFarm` names are single path
  # components, so the two subdirectories are created directly rather than
  # through a farm.
  devWallpaperDirectory =
    pkgs.runCommand "genkan-wallpapers"
      {
        nativeBuildInputs = [ pkgs.coreutils ];
      }
      ''
        mkdir -p $out/mov $out/heic
        ${pkgs.lib.concatMapStringsSep "\n" (wallpaper: ''
          ln -s ${wallpaper.videoSource} "$out/mov/${wallpaper.install_name}"
        '') wallpapers}
        ${pkgs.lib.concatMapStringsSep "\n" (asset: ''
          ln -s ${asset.source} "$out/heic/${asset.install_name}"
        '') dynamicHeicAssets}
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
    buildInputs = gstreamerPackages ++ [
      pkgs.libheif
      pkgs.libxkbcommon
      pkgs.pam
    ];
    postInstall = ''
      wallpaperDirectory=$out/share/genkan/wallpapers
      mkdir -p "$wallpaperDirectory/mov" "$wallpaperDirectory/heic"
      install -m 0444 ${../assets/wallpapers/manifest.toml} "$wallpaperDirectory/manifest.toml"
      ${pkgs.lib.concatMapStringsSep "\n" installWallpaper wallpapers}
      ${pkgs.lib.concatMapStringsSep "\n" installHeicAsset dynamicHeicAssets}

      wrapProgram $out/bin/genkan \
        --set FONTCONFIG_FILE ${fontConfig} \
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
      pkgs.git-cliff
      pkgs.jq
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
    GENKAN_WALLPAPER_DIR = devWallpaperDirectory;
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
    inherit pkgs;
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
  solarAppConfig = solarModuleSystem.config.services.geoclue2.appConfig."genkan-wallpaper";
  solarGeoclueConfig = pkgs.writeText "genkan-geoclue-config"
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
    assert solarModuleSystem.config.services.geoclue2.whitelistedAgents
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
  # Decodes the installed assets rather than their sources, so the check covers
  # the packaged symlinks, the shipped container preflight, the metadata parser,
  # and tiled decoding end to end. A regression that only breaks real multi-image
  # wallpapers fails the build here instead of only in the graphical smoke test.
  # Every catalog asset must decode: an entry that records
  # `decode_verified = false` fails the check instead of dropping out of it, so
  # re-introducing an exception has to change this check and the manifest entry
  # together rather than silently reducing what is verified.
  heicDecodeExcludedAssets = builtins.filter (asset: !(asset.decode_verified or true)) dynamicHeicAssets;
  heicDecodeCheck =
    assert builtins.length dynamicHeicAssets > 0;
    assert builtins.length heicDecodeExcludedAssets == 0;
    pkgs.runCommand "genkan-heic-decode-check"
      {
        nativeBuildInputs = [ pkgs.coreutils ];
      }
      ''
        ${pkgs.lib.concatMapStringsSep "\n" (asset: ''
          ${package}/bin/genkan verify-wallpapers \
            --file ${package}/share/genkan/wallpapers/heic/${asset.install_name} \
            --expect-frames ${toString asset.structure.image_count}
        '') dynamicHeicAssets}
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
    heic-assets = heicAssetCheck;
    heic-decode = heicDecodeCheck;
    graphics-smoke = import ./tests/graphics-smoke.nix {
      inherit pkgs;
      genkan = package;
    };
    login-output-smoke = import ./tests/login-output-smoke.nix {
      inherit pkgs;
      genkan = package;
    };
    desktop-wallpaper-smoke = import ./tests/desktop-wallpaper-smoke.nix {
      inherit pkgs;
      genkan = package;
      lockTestGenkan = sessionLockTestPackage;
    };
    preview-evidence = import ./tests/preview-evidence.nix {
      inherit pkgs;
      genkan = package;
    };
    session-lock-smoke = import ./tests/session-lock-smoke.nix {
      inherit pkgs;
      genkan = sessionLockTestPackage;
      productionGenkan = package;
    };
  }
  // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
    greetd-e2e = pkgs.testers.runNixOSTest (import ./tests/greetd.nix { genkanE2e = e2ePackage; });
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
