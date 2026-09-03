{
  pkgs,
  genkan,
  checkBaseline ? true,
}:

pkgs.runCommand "genkan-preview-evidence${pkgs.lib.optionalString (!checkBaseline) "-capture"}"
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
    export PREVIEW_EVIDENCE_LIB=${../../scripts/preview-evidence-lib.sh}
    ${pkgs.lib.optionalString checkBaseline ''
      export PREVIEW_REFERENCE_DIR=${../../rfd/0001/reference-images}
      export PREVIEW_REFERENCE_MANIFEST=${../../scripts/reference-images-manifest.sh}
    ''}
    export VK_DRIVER_FILES=${pkgs.mesa}/share/vulkan/icd.d/lvp_icd.${pkgs.stdenv.hostPlatform.parsed.cpu.name}.json
    bash ${../../scripts/capture-preview-evidence.sh}
  ''
