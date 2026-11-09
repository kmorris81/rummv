{
  description = "mview — Rust image & video viewer (Windows Photos-style folder browsing)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      runtimeLibs = pkgs: with pkgs; [
        libGL
        libxkbcommon
        wayland
        libx11
        libxcursor
        libxi
        libxrandr
      ];
    in
    {
      packages = forAll (pkgs: {
        default = pkgs.rustPlatform.buildRustPackage {
          pname = "mview";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [ pkg-config makeWrapper rustPlatform.bindgenHook ];
          buildInputs = with pkgs; [ mpv-unwrapped libheif ] ++ runtimeLibs pkgs;

          # Fixture-based tests need ffmpeg etc.; run them via `nix develop -c cargo test`.
          doCheck = false;

          postInstall = ''
            install -Dm644 assets/mview.desktop $out/share/applications/mview.desktop
          '';

          # winit/glutin dlopen GL, Wayland and X11 libraries at runtime.
          # ffmpegthumbnailer/ffmpeg generate filmstrip thumbnails for videos.
          postFixup = ''
            wrapProgram $out/bin/mview \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (runtimeLibs pkgs)} \
              --suffix LD_LIBRARY_PATH : /run/opengl-driver/lib \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.ffmpegthumbnailer pkgs.ffmpeg-headless ]}
          '';

          meta = {
            description = "Image & video viewer with arrow-key folder navigation";
            mainProgram = "mview";
            platforms = pkgs.lib.platforms.linux;
          };
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ffmpeg ffmpegthumbnailer ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (runtimeLibs pkgs);
        };
      });
    };
}
