use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"));
    let dist = root.join("frontend/dist");
    println!("cargo:rerun-if-changed=frontend/dist");
    assert!(
        dist.join("index.html").is_file(),
        "The React frontend is not built. Run `npm ci --prefix frontend` and `npm run build --prefix frontend` before Cargo."
    );

    let mut files = vec![dist.join("index.html")];
    for name in ["avatar.jpg", "appearance.js"] {
        let path = dist.join(name);
        assert!(path.is_file(), "Missing built frontend asset: {name}");
        files.push(path);
    }
    collect_assets(&dist.join("assets"), &mut files);
    files.sort();

    let mut generated = String::from("static ASSETS: &[Asset] = &[\n");
    for file in files {
        let relative = file
            .strip_prefix(&dist)
            .expect("frontend asset stays inside dist")
            .to_str()
            .expect("frontend asset names are UTF-8")
            .replace('\\', "/");
        assert!(
            relative
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'/' | b'-' | b'_' | b'.')),
            "Unsupported frontend asset name: {relative}"
        );
        let content_type = match file.extension().and_then(|value| value.to_str()) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("svg") => "image/svg+xml",
            Some("webp") => "image/webp",
            Some("ico") => "image/x-icon",
            Some("woff") => "font/woff",
            Some("woff2") => "font/woff2",
            Some("ttf") => "font/ttf",
            _ => continue,
        };
        let hashed = relative.starts_with("assets/")
            && file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    // Vite's eight-character hashes can themselves contain '-'.
                    stem.len() > 9
                        && stem.as_bytes()[stem.len() - 9] == b'-'
                        && stem[stem.len() - 8..]
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                });
        println!("cargo:rerun-if-changed={}", file.display());
        generated.push_str(&format!(
            "Asset {{ path: {:?}, content_type: {:?}, bytes: include_bytes!({:?}), immutable: {hashed} }},\n",
            format!("/{relative}"), content_type, file
        ));
    }
    generated.push_str("];");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    fs::write(out.join("frontend_assets.rs"), generated).expect("write frontend asset manifest");
}

fn collect_assets(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("read Vite assets directory") {
        let entry = entry.expect("read frontend asset entry");
        let kind = entry.file_type().expect("read frontend asset type");
        assert!(!kind.is_symlink(), "Frontend assets must not be symlinks");
        if kind.is_dir() {
            collect_assets(&entry.path(), files);
        } else if kind.is_file() {
            files.push(entry.path());
        }
    }
}
