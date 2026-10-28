//! The Vite build is embedded at compile time; requests never read the filesystem.
use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

struct Asset {
    path: &'static str,
    content_type: &'static str,
    bytes: &'static [u8],
    immutable: bool,
}

include!(concat!(env!("OUT_DIR"), "/frontend_assets.rs"));

pub async fn index() -> Response {
    response("/index.html")
}

pub async fn asset(Path(path): Path<String>) -> Response {
    response(&format!("/assets/{path}"))
}

pub async fn avatar() -> Response {
    response("/avatar.jpg")
}

pub async fn appearance() -> Response {
    response("/appearance.js")
}

pub async fn not_found() -> Response {
    response("")
}

pub fn stylesheet_links() -> String {
    ASSETS
        .iter()
        .filter(|asset| asset.path.starts_with("/assets/") && asset.path.ends_with(".css"))
        .map(|asset| format!("<link rel=\"stylesheet\" href=\"{}\">", asset.path))
        .collect()
}

fn response(path: &str) -> Response {
    let found = if path.contains("..") || path.contains('\\') || path.contains('%') {
        None
    } else {
        ASSETS.iter().find(|asset| asset.path == path)
    };
    match found {
        Some(asset) => (
            [
                (header::CONTENT_TYPE, asset.content_type),
                (
                    header::CACHE_CONTROL,
                    if asset.immutable {
                        "public, max-age=31536000, immutable"
                    } else {
                        "no-cache"
                    },
                ),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            asset.bytes,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            "Not found",
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_assets_use_the_expected_content_types_and_cache_policy() {
        for path in ["/index.html", "/avatar.jpg", "/appearance.js"] {
            let served = response(path);
            assert_eq!(served.status(), StatusCode::OK);
            assert_eq!(served.headers()[header::CACHE_CONTROL], "no-cache");
            assert_eq!(served.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        }
        assert_eq!(
            response("/avatar.jpg").headers()[header::CONTENT_TYPE],
            "image/jpeg"
        );
        let stylesheet = ASSETS
            .iter()
            .find(|asset| asset.path.ends_with(".css"))
            .unwrap();
        assert_eq!(
            response(stylesheet.path).headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert!(stylesheet_links().contains(stylesheet.path));
    }

    #[test]
    fn only_manifest_entries_can_be_served() {
        for path in [
            "/assets/missing.js",
            "/assets/../../Cargo.toml",
            "/assets/%2e%2e/Cargo.toml",
            "/assets/..\\Cargo.toml",
            "/src/main.rs",
            "/.env",
        ] {
            let served = response(path);
            assert_eq!(served.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(served.headers()[header::CACHE_CONTROL], "no-store");
        }
    }
}
