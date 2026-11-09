# Builds rummv from source. package-bin.nix installs the same thing prebuilt.
{
  lib,
  pkgs,
  rustPlatform,
  pkg-config,
  makeWrapper,
}:

let
  runtime = import ./nix/runtime.nix { inherit lib pkgs; };
in

rustPlatform.buildRustPackage {
  pname = "rummv";
  inherit ((lib.importTOML ./Cargo.toml).package) version;

  # Only the files cargo needs, so a local target/ never ends up in the build.
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./assets
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [ pkg-config makeWrapper rustPlatform.bindgenHook ];
  buildInputs = runtime.linkedLibs ++ runtime.dlopenLibs;

  # Fixture tests need tests/fixtures (left out of src); run them via `nix develop -c cargo test`.
  doCheck = false;

  postInstall = ''
    install -Dm644 assets/rummv.desktop $out/share/applications/rummv.desktop
    install -Dm644 assets/rummv.svg $out/share/icons/hicolor/scalable/apps/rummv.svg
    install -Dm644 assets/rummv.png $out/share/icons/hicolor/256x256/apps/rummv.png
  '';

  postFixup = runtime.wrap;

  meta = runtime.meta // {
    platforms = lib.platforms.linux;
  };
}
