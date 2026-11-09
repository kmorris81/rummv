# What rummv needs at runtime, shared by the source build (package.nix) and the
# prebuilt release binary (package-bin.nix).
{ lib, pkgs }:

rec {
  # winit/glutin dlopen GL, Wayland and X11 libraries at runtime.
  dlopenLibs = with pkgs; [
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ];

  # Linked directly by the binary.
  linkedLibs = with pkgs; [ mpv-unwrapped libheif ];

  # ffmpegthumbnailer/ffmpeg generate filmstrip thumbnails for videos.
  wrap = ''
    wrapProgram $out/bin/rummv \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath dlopenLibs} \
      --suffix LD_LIBRARY_PATH : /run/opengl-driver/lib \
      --prefix PATH : ${lib.makeBinPath [ pkgs.ffmpegthumbnailer pkgs.ffmpeg-headless ]}
  '';

  meta = {
    description = "Image & video viewer with arrow-key folder navigation";
    homepage = "https://github.com/kmorris81/rummv";
    mainProgram = "rummv";
  };
}
