use std::path::PathBuf;

fn main() {
    slint_build::compile("ui/app.slint").expect("Slint build failed");
    bundle_ytdlp();
}

/// Fetches the standalone yt-dlp for the target OS once (cached in vendor/,
/// gitignored) and hands its path to `include_bytes!` in src/ytdlp.rs, so the
/// app ships with yt-dlp inside its own binary and never downloads it at runtime.
/// Pin a release with NULLYT_YTDLP_VERSION=2026.10.26.
fn bundle_ytdlp() {
    let asset = match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("windows") => "yt-dlp.exe",
        Ok("macos") => "yt-dlp_macos",
        _ => "yt-dlp_linux",
    };
    let dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("vendor");
    let path = dir.join(asset);
    println!("cargo:rerun-if-changed={}", path.display());
    println!("cargo:rerun-if-env-changed=NULLYT_YTDLP_VERSION");

    if !path.exists() {
        let version = std::env::var("NULLYT_YTDLP_VERSION").unwrap_or_else(|_| "latest".into());
        let url = if version == "latest" {
            format!("https://github.com/yt-dlp/yt-dlp/releases/latest/download/{asset}")
        } else {
            format!("https://github.com/yt-dlp/yt-dlp/releases/download/{version}/{asset}")
        };
        std::fs::create_dir_all(&dir).unwrap();
        let res = ureq::get(&url)
            .call()
            .unwrap_or_else(|e| panic!("download {url}: {e}"));
        let tmp = path.with_extension("part");
        let mut f = std::fs::File::create(&tmp).unwrap();
        std::io::copy(&mut res.into_body().into_reader(), &mut f).unwrap();
        drop(f);
        std::fs::rename(&tmp, &path).unwrap();
    }
    println!("cargo:rustc-env=NULLYT_YTDLP_PATH={}", path.display());
}
