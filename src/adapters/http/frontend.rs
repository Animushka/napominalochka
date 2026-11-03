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

pub(crate) async fn index() -> Response {
    response("/index.html")
}

pub(crate) async fn asset(Path(path): Path<String>) -> Response {
    response(&format!("/assets/{path}"))
}

pub(crate) async fn avatar() -> Response {
    response("/avatar.jpg")
}

pub(crate) async fn appearance() -> Response {
    response("/appearance.js")
}

pub(crate) async fn not_found() -> Response {
    response("")
}

pub(crate) fn stylesheet_links() -> String {
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
mod tests;
