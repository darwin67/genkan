{ pkgs, genkan }:

pkgs.runCommand "genkan-preview-evidence"
  {
    FONTCONFIG_FILE = pkgs.makeFontsConf {
      fontDirectories = [ pkgs.dejavu_fonts ];
    };
    nativeBuildInputs = [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.imagemagick
      pkgs.strace
      pkgs.util-linux
      pkgs.weston
    ];
  }
  ''
    export GENKAN_BIN=${genkan}/bin/genkan
    export PREVIEW_OUTPUT_DIR=$out
    export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json
    bash ${../../scripts/capture-preview-evidence.sh}
  ''
