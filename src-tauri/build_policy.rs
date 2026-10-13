pub fn local_debug(profile: &str, channel: &str, url: &str) -> bool {
    profile == "debug"
        && channel == "local-dev"
        && matches!(url, "http://127.0.0.1:54321" | "http://localhost:54321")
}

pub fn target_allowed(profile: &str, channel: &str, url: &str, expected: &str) -> bool {
    match channel {
        "local-dev" => local_debug(profile, channel, url),
        "production" | "preview" => url == expected,
        _ => false,
    }
}
