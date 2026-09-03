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
    buildInputs = gstreamerPackages;
    postInstall = ''
      wallpaperDirectory=$out/share/genkan/wallpapers
      mkdir -p "$wallpaperDirectory"
      install -m 0444 ${../assets/wallpapers/manifest.toml} "$wallpaperDirectory/manifest.toml"
      ${pkgs.lib.concatMapStringsSep "\n" installWallpaper wallpapers}

      wrapProgram $out/bin/genkan \
        --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath runtimeLibraries} \
        --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${gstreamerPluginPath} \
        --suffix VK_ADD_DRIVER_FILES : ${pkgs.addDriverRunpath.driverLink}/share/vulkan/icd.d
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

  devShell = pkgs.mkShell {
    packages = [
      pkgs.awscli2
      pkgs.git-cliff
      pkgs.jq
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
      export FONTCONFIG_FILE=${pkgs.makeFontsConf { fontDirectories = [ pkgs.dejavu_fonts ]; }}
      ${builtins.readFile ../scripts/hardware-smoke.sh}
    '';
  };
  previewEvidenceCapture = import ./tests/preview-evidence.nix {
    inherit pkgs;
    genkan = package;
    checkBaseline = false;
  };
in
{
  inherit package devShell previewEvidenceCapture;

  hardwareSmokeApp = {
    type = "app";
    program = "${hardwareSmoke}/bin/genkan-hardware-smoke";
  };

  checks = {
    inherit package;
    graphics-smoke = import ./tests/graphics-smoke.nix {
      inherit pkgs;
      genkan = package;
    };
    preview-evidence = import ./tests/preview-evidence.nix {
      inherit pkgs;
      genkan = package;
    };
  }
  // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
    greetd-e2e = pkgs.testers.runNixOSTest (import ./tests/greetd.nix { genkanE2e = e2ePackage; });
  };
}
