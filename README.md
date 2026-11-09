# rummv (RUst Multi-Media Viewer)

A fast image & video viewer in Rust that behaves like the Windows Photos app:
open any file from Dolphin and use **← / →** to step through every image and video in
that folder. Videos autoplay (with sound, looping).

* **Images:** JPEG, PNG, APNG, GIF, WebP (incl. animated), BMP, ICO, TIFF, TGA, DDS,
  HDR, EXR, PNM, QOI, HEIC/HEIF, AVIF, JPEG XL, SVG, PSD, ICNS, and camera RAW (CR2/CR3/NEF/ARW/
  DNG/RAF/ORF/RW2/…). EXIF orientation is applied automatically.
* **Video:** anything mpv/ffmpeg plays (MP4, MKV, WebM, MOV, AVI, …) via embedded libmpv.
* **iPhone slow motion:** 120/240 fps slo-mo clips play their slow section slowed down, like
  Photos does. The section comes from the clip's `.AAE` sidecar (your Photos edit) when it's
  next to the video, otherwise Apple's default (15%–85% of the clip). **S** toggles it.
  Zero-copy hardware decoding (`hwdec=auto-safe`), automatic CPU fallback, Ctrl+H toggle.
* Natural sort (same order as Dolphin), wrap-around, live folder updates, neighbour
  prefetch, filmstrip (shares Dolphin's `~/.cache/thumbnails`), info panel with EXIF/codec.
* **Anything that modifies or deletes a file asks for confirmation first.**

Press **F1** in the app for all shortcuts.

## Install on NixOS (flake)

In your system flake:

```nix
inputs.rummv.url = "github:kmorris81/rummv";
inputs.rummv.inputs.nixpkgs.follows = "nixpkgs";
# ...
environment.systemPackages = [ inputs.rummv.packages.${pkgs.stdenv.hostPlatform.system}.default ];
```

`default` installs the prebuilt binary from the latest GitHub release when `release.json`
matches the version in `Cargo.toml` (x86_64 only), so nothing compiles on your machine;
otherwise it builds from source. `.#rummv-src` always builds from source.

Try it first without installing (`nix run` / `nix shell`):

```sh
nix run /path/to/rummv -- ~/Pictures/some.jpg
```

## Make it the default viewer in Dolphin

System Settings → **Default Applications** → *Image viewer* → rummv,
or per type: right-click a file in Dolphin → **Open With → Other Application…** → rummv →
tick *Remember application association*. The bundled `rummv.desktop` registers all image
and video MIME types (`Exec=rummv %f`; rummv derives the folder from the file).

## Releasing

Bump `version` in `Cargo.toml`, commit, then tag and push:

```sh
git tag v0.2.0 && git push origin main v0.2.0
```

`.github/workflows/release.yml` runs the tests, builds with Nix, publishes
`rummv-<version>-x86_64-linux.tar.gz` as a GitHub release and commits an updated
`release.json` to `main` (so `git pull` afterwards).

## Develop

```sh
nix develop
cargo test        # unit + decoder fixture tests (tests/fixtures)
cargo run -- tests/fixtures/a01.jpg
```

## Config

`~/.config/rummv/config.toml` (written on exit; volume/mute are remembered):

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
slowmo = true           # iPhone slow-motion playback (S toggles)
```

### Passing options to mpv

Anything under `[mpv]` is handed to libmpv as-is (applied after rummv's defaults; invalid
options are logged and skipped). rummv defaults to `tone-mapping = "hable"` for HDR video
(iPhone footage etc.); override it here if you prefer another curve:

```toml
[mpv]
tone-mapping = "bt.2446a"
hdr-compute-peak = "no"
```

Run with `RUMMV_MPV_LOG=v rummv file.mov` to see mpv's own log on stderr.
