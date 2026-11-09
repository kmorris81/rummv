# The prebuilt binary from the GitHub release named in release.json (written by
# .github/workflows/release.yml), so nothing is compiled on the target machine.
# CI builds it with Nix; autoPatchelfHook relinks it against this nixpkgs'
# libmpv/libheif/glibc. package.nix builds the same thing from source.
{
  lib,
  pkgs,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  makeWrapper,
}:

let
  release = lib.importJSON ./release.json;
  runtime = import ./nix/runtime.nix { inherit lib pkgs; };
in

stdenv.mkDerivation {
  pname = "rummv";
  inherit (release) version;

  src = fetchurl { inherit (release) url hash; };

  nativeBuildInputs = [ autoPatchelfHook makeWrapper ];
  buildInputs = runtime.linkedLibs ++ [ stdenv.cc.cc.lib ];

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    mkdir -p $out
    cp -r bin share $out/
  '';

  postFixup = runtime.wrap;

  meta = runtime.meta // {
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
}
