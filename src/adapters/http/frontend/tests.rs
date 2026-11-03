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
