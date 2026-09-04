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
  installWallpaper = wallpaper: ''
    ln -s ${wallpaper.videoSource} "$wallpaperDirectory/${wallpaper.install_name}"
    ln -s ${wallpaper.posterSource} "$wallpaperDirectory/${wallpaper.poster.file}"
  '';
  devWallpaperDirectory = pkgs.linkFarm "genkan-wallpapers" (
    map (wallpaper: {
      name = wallpaper.install_name;
      path = wallpaper.videoSource;
    }) wallpapers
  );

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
      pkgs.libxkbcommon
      pkgs.pam
    ];
    postInstall = ''
      wallpaperDirectory=$out/share/genkan/wallpapers
      mkdir -p "$wallpaperDirectory"
      install -m 0444 ${../assets/wallpapers/manifest.toml} "$wallpaperDirectory/manifest.toml"
      ${pkgs.lib.concatMapStringsSep "\n" installWallpaper wallpapers}

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
      pkgs.libxkbcommon
      pkgs.pam
      pkgs.pkg-config
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
  modulePamPolicy = pkgs.writeText "genkan-lock-pam-policy" (
    moduleSystem.config.security.pam.services.genkan-lock.text
  );
  moduleCheck =
    assert builtins.elem package moduleSystem.config.environment.systemPackages;
    pkgs.runCommand "genkan-module-check" { nativeBuildInputs = [ pkgs.gnugrep ]; } ''
      grep -F 'pam_unix.so' ${modulePamPolicy}
      grep -F 'pam_deny.so' ${modulePamPolicy}
      ! grep -F 'pam_permit.so' ${modulePamPolicy}
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
    graphics-smoke = import ./tests/graphics-smoke.nix {
      inherit pkgs;
      genkan = package;
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
  };
}
