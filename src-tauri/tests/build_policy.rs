#[path = "../build_policy.rs"]
mod policy;

#[test]
fn local_build_is_debug_only_and_cannot_reuse_hosted_channels() {
    for url in ["http://127.0.0.1:54321", "http://localhost:54321"] {
        assert!(policy::target_allowed("debug", "local-dev", url, url));
        assert!(!policy::target_allowed("release", "local-dev", url, url));
        assert!(!policy::target_allowed(
            "debug",
            "production",
            url,
            "https://prod.example"
        ));
        assert!(!policy::target_allowed(
            "debug",
            "preview",
            url,
            "https://preview.example"
        ));
    }
    for url in [
        "http://localhost:54322",
        "http://localhost.evil:54321",
        "http://localhost:54321/",
        "https://prod.example",
    ] {
        assert!(!policy::local_debug("debug", "local-dev", url));
    }
    assert!(policy::target_allowed(
        "release",
        "production",
        "https://prod.example",
        "https://prod.example"
    ));
    assert!(policy::target_allowed(
        "release",
        "preview",
        "https://preview.example",
        "https://preview.example"
    ));
    assert!(!policy::target_allowed(
        "debug",
        "unknown",
        "https://prod.example",
        "https://prod.example"
    ));
}
