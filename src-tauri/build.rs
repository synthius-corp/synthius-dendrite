use base64::Engine;
use serde_json::Value;
use std::{env, fs, path::PathBuf};
mod build_policy;

fn main() {
    for key in [
        "SYNTHIUS_RELEASE_CHANNEL",
        "SYNTHIUS_SUPABASE_URL",
        "SYNTHIUS_SUPABASE_PUBLIC_KEY",
        "TAURI_CONFIG",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    println!("cargo:rerun-if-changed=../distribution/profiles.json");
    println!("cargo:rerun-if-changed=build_policy.rs");
    let profiles: Value =
        serde_json::from_str(&fs::read_to_string("../distribution/profiles.json").unwrap())
            .unwrap();
    let channel = env::var("SYNTHIUS_RELEASE_CHANNEL").unwrap_or_else(|_| "production".into());
    let mut profile = profiles
        .get(&channel)
        .expect("Unknown desktop release channel")
        .clone();
    if channel == "preview" {
        println!("cargo:rerun-if-env-changed=SYNTHIUS_PREVIEW_CONFIG_FILE");
        let file = env::var("SYNTHIUS_PREVIEW_CONFIG_FILE")
            .expect("Maintainer Preview builds require a private SYNTHIUS_PREVIEW_CONFIG_FILE");
        let repository = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap();
        assert!(
            !PathBuf::from(&file)
                .canonicalize()
                .unwrap()
                .starts_with(repository),
            "Keep private Preview configuration outside the repository"
        );
        println!("cargo:rerun-if-changed={file}");
        let private: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        for field in ["supabaseUrl", "webAppUrl", "backendUrl"] {
            let value = private[field]
                .as_str()
                .expect("Missing private Preview endpoint");
            assert!(
                value.starts_with("https://"),
                "Preview endpoints require HTTPS"
            );
            profile[field] = Value::String(value.into());
        }
    }
    let expected_url = profile["supabaseUrl"].as_str().unwrap();
    let url = env::var("SYNTHIUS_SUPABASE_URL").unwrap_or_else(|_| expected_url.into());
    let build_profile = env::var("PROFILE").unwrap_or_default();
    let local_debug = build_policy::local_debug(&build_profile, &channel, &url);
    assert!(
        build_policy::target_allowed(&build_profile, &channel, &url, expected_url),
        "Supabase project does not match the desktop channel"
    );
    {
        let key = env::var("SYNTHIUS_SUPABASE_PUBLIC_KEY")
            .expect("Set SYNTHIUS_SUPABASE_PUBLIC_KEY for the selected desktop environment");
        assert!(
            !key.trim().is_empty(),
            "Desktop requires a public Supabase key"
        );
        if let Some(suffix) = key.strip_prefix("sb_publishable_") {
            assert!(
                !suffix.is_empty()
                    && suffix
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == b'_' || ch == b'-'),
                "Desktop requires a valid public Supabase key"
            );
        } else {
            assert_eq!(
                key.split('.').count(),
                3,
                "Desktop requires a public Supabase key"
            );
            let claims = key
                .split('.')
                .nth(1)
                .and_then(|value| {
                    base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(value)
                        .ok()
                })
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .expect("Desktop requires a public Supabase key");
            assert_eq!(
                claims["role"], "anon",
                "Privileged Supabase keys cannot enter a desktop build"
            );
            if !local_debug {
                let project = expected_url
                    .strip_prefix("https://")
                    .unwrap()
                    .split('.')
                    .next()
                    .unwrap();
                assert_eq!(
                    claims["ref"], project,
                    "Public key belongs to another Supabase project"
                );
            }
        }
    }
    let config: Value = env::var("TAURI_CONFIG")
        .ok()
        .map(|value| serde_json::from_str(&value).expect("Invalid Tauri overlay"))
        .unwrap_or_else(|| serde_json::json!({}));
    let base: Value =
        serde_json::from_str(&fs::read_to_string("tauri.conf.json").unwrap()).unwrap();
    for field in ["identifier", "productName"] {
        assert_eq!(
            config.get(field).unwrap_or(&base[field]),
            &profile[field],
            "Desktop installer identity does not match its channel"
        );
    }
    let constants = format!(
        "const RELEASE_CHANNEL: &str = {channel:?};\nconst PRODUCT_NAME: &str = {name:?};\nconst DATA_DIRECTORY: &str = {directory:?};\nconst DEFAULT_PORT: u16 = {port};\nconst WEB_APP_URL: &str = {web:?};\nconst BACKEND_URL: &str = {backend:?};\n",
        name = profile["productName"].as_str().unwrap(),
        directory = profile["dataDirectory"].as_str().unwrap(),
        port = profile["port"].as_u64().unwrap(),
        web = profile["webAppUrl"].as_str().unwrap(),
        backend = profile["backendUrl"].as_str().unwrap(),
    );
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("distribution.rs"),
        constants,
    )
    .unwrap();
    println!("cargo:rustc-env=SYNTHIUS_SUPABASE_URL={url}");
    tauri_build::build()
}
