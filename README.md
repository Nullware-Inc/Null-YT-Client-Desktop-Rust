# Null YT — Desktop (Rust + Slint)

Native Rust client for Null YT, built with [Slint](https://slint.dev).
It uses the same yt-dlp command lines, settings and YouTube sync as the Dart clients.

- **Solid / liquid glass** surfaces (`ui/app.slint` → `Surface`), animated aurora background
- Windows 11 **Mica / Acrylic** window material via `window-vibrancy`
- yt-dlp is **embedded in the binary** (fetched once at build time by `build.rs` into `vendor/`), written to the app data folder on first run and auto-updated from there
- YouTube sync through your browser session (`--cookies-from-browser`) or an imported cookies.txt
- Playback through **mpv** (set the player path in Settings). Embedded playback is planned.

```bash
cargo run
```

Slint has no backdrop blur of in-window content. The glass look comes from translucent
lit panels over the aurora, plus real OS blur when Mica or Acrylic is enabled.
