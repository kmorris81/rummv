{
  description = "rummv — Rust image & video viewer (Windows Photos-style folder browsing)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Use the GitHub release binary only if release.json describes this very
      # version (and we're on x86_64); otherwise build from source.
      havePrebuilt = system:
        system == "x86_64-linux"
        && builtins.pathExists ./release.json
        && (lib.importJSON ./release.json).version == (lib.importTOML ./Cargo.toml).package.version;
    in
    {
      packages = forAll (pkgs:
        let
          system = pkgs.stdenv.hostPlatform.system;
          src = pkgs.callPackage ./package.nix { };
        in
        {
          rummv-src = src;
          default = if havePrebuilt system then pkgs.callPackage ./package-bin.nix { } else src;
        });

      devShells = forAll (pkgs:
        let runtime = import ./nix/runtime.nix { inherit lib pkgs; };
        in {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.rummv-src ];
            packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ffmpeg ffmpegthumbnailer ];
            LD_LIBRARY_PATH = lib.makeLibraryPath runtime.dlopenLibs;
          };
        });
    };
}
