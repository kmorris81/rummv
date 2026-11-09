# mview

A fast image & video viewer in Rust that behaves like the Windows Photos app:
open any file from Dolphin and use **← / →** to step through every image and video in
that folder. Videos autoplay (with sound, looping).

* **Images:** JPEG, PNG, APNG, GIF, WebP (incl. animated), BMP, ICO, TIFF, TGA, DDS,
  HDR, EXR, PNM, QOI, HEIC/HEIF, AVIF, JPEG XL, SVG, PSD, and camera RAW (CR2/CR3/NEF/ARW/
  DNG/RAF/ORF/RW2/…). EXIF orientation is applied automatically.
* **Video:** anything mpv/ffmpeg plays (MP4, MKV, WebM, MOV, AVI, …) via embedded libmpv.
  Zero-copy hardware decoding (`hwdec=auto-safe`), automatic CPU fallback, Ctrl+H toggle.
* Natural sort (same order as Dolphin), wrap-around, live folder updates, neighbour
  prefetch, filmstrip (shares Dolphin's `~/.cache/thumbnails`), info panel with EXIF/codec.
* **Anything that modifies or deletes a file asks for confirmation first.**

Press **F1** in the app for all shortcuts.

## Install on NixOS (flake)

In your system flake:

```nix
inputs.mview.url = "path:/path/to/mview";   # or a git URL
# ...
environment.systemPackages = [ inputs.mview.packages.${pkgs.system}.default ];
```

Try it first without installing (`nix run` / `nix shell`):

```sh
nix run /path/to/mview -- ~/Pictures/some.jpg
```

## Make it the default viewer in Dolphin

System Settings → **Default Applications** → *Image viewer* → mview,
or per type: right-click a file in Dolphin → **Open With → Other Application…** → mview →
tick *Remember application association*. The bundled `mview.desktop` registers all image
and video MIME types (`Exec=mview %f`; mview derives the folder from the file).

## Develop

```sh
nix develop
cargo test        # unit + decoder fixture tests (tests/fixtures)
cargo run -- tests/fixtures/a01.jpg
```

## Config

`~/.config/mview/config.toml` (written on exit; volume/mute are remembered):

```toml
volume = 70.0
mute = false
hwdec = "auto-safe"     # mpv hwdec value used when HW decoding is on
hw_decode = true        # Ctrl+H toggles at runtime
fit_mode = "fit"        # fit | fill | actual
start_fullscreen = false
watch_folder = true
show_hidden = false
show_filmstrip = false
show_info = false
slideshow_secs = 4.0
loop_video = true
```
