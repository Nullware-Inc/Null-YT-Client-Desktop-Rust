# Null YT — Desktop (Rust + Slint)

Native Rust client for Null YT, built with [Slint](https://slint.dev).
It uses the same yt-dlp command lines, settings and YouTube sync as the Dart clients.

- **Solid / liquid glass** surfaces (`ui/app.slint` → `Surface`), animated aurora background
- Windows 11 **Mica / Acrylic** window material via `window-vibrancy`
- yt-dlp is **embedded in the binary** (fetched once at build time by `build.rs` into `vendor/`), written to the app data folder on first run and auto-updated from there
- YouTube sync through your browser session (`--cookies-from-browser`) or an imported cookies.txt
- **For you without an account** (`src/foryou.rs`): Home builds a session feed from what you watch, search and skip, using YouTube's related videos fetched signed out. Same algorithm as the Dart apps. Hover a card for **Not interested** (or right-click it); **Topics** gives it a head start. Signed in, switch Home between your YouTube feed and the session mix.
- **Built-in player** (`src/player.rs`): libmpv loaded at runtime and drawn into the window with mpv's software renderer. Big centre controls, chapter-split progress bar, captions, quality / speed / sleep timer sheet, Up next card, fullscreen, keyboard (Space/K, J/L or arrows, F, C, N/P, Esc).
  - Windows release zips ship `libmpv-2.dll` next to the exe. For local builds, put it there yourself (from [shinchiro/mpv-winbuild-cmake](https://github.com/shinchiro/mpv-winbuild-cmake/releases), `mpv-dev-x86_64-*.7z`).
  - Linux uses the system libmpv (`libmpv2` on Debian/Ubuntu, `mpv-libs` on Fedora, `mpv` on Arch); macOS uses Homebrew's `mpv`.
  - Without libmpv, or with *Built-in player* off, videos open in an external **mpv** (set its command in Settings).

```bash
cargo run
```

Slint has no backdrop blur of in-window content. The glass look comes from translucent
lit panels over the aurora, plus real OS blur when Mica or Acrylic is enabled.
