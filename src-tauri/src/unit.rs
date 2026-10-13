// Offline sidecar unit tests, included only under cfg(test).
use super::*;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

// Synthetic PKCS#8 fixture encoded as DER rather than a PEM credential. It is
// public test material only and cannot authenticate to any Synthius service.
const TEST_ES256_PRIVATE_KEY_DER: &[u8] = &[
    0x30, 0x81, 0x87, 0x02, 0x01, 0x00, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02,
    0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x04, 0x6d, 0x30, 0x6b, 0x02,
    0x01, 0x01, 0x04, 0x20, 0xc0, 0x80, 0x2a, 0xae, 0xe3, 0x3f, 0x1e, 0x3e, 0xed, 0xf0, 0x4d, 0xb1,
    0x6c, 0x08, 0x89, 0xb8, 0xa7, 0xab, 0x5d, 0x9c, 0x89, 0x55, 0xba, 0xbe, 0x3b, 0x18, 0xad, 0x5f,
    0x34, 0x95, 0x1c, 0x06, 0xa1, 0x44, 0x03, 0x42, 0x00, 0x04, 0x7f, 0xb0, 0x59, 0x48, 0x7c, 0x17,
    0x37, 0xb6, 0x88, 0x04, 0xee, 0x45, 0x7c, 0x69, 0x14, 0x42, 0x10, 0x30, 0x57, 0x3e, 0xdb, 0xc0,
    0x42, 0xe4, 0x9f, 0xe8, 0x15, 0xbf, 0xc8, 0xc4, 0xce, 0x18, 0x2c, 0x30, 0x17, 0xb2, 0xc6, 0xad,
    0xb9, 0xc5, 0xf7, 0x92, 0x49, 0x5d, 0xa9, 0x08, 0x67, 0xeb, 0x5d, 0xf5, 0xd4, 0x93, 0x6a, 0x1e,
    0x36, 0x3f, 0xc6, 0x30, 0x20, 0x21, 0x55, 0x35, 0xfe, 0x80,
];

fn test_es256_jwk() -> jsonwebtoken::jwk::Jwk {
    serde_json::from_value(json!({
        "kty": "EC",
        "kid": "test-key",
        "use": "sig",
        "alg": "ES256",
        "crv": "P-256",
        "x": "f7BZSHwXN7aIBO5FfGkUQhAwVz7bwELkn-gVv8jEzhg",
        "y": "LDAXssatucX3kkldqQhn61311JNqHjY_xjAgIVU1_oA"
    }))
    .expect("valid test JWK")
}

#[derive(Serialize)]
struct TestClaims<'a> {
    sub: &'a str,
    role: &'a str,
    aud: &'a str,
    iss: String,
    exp: u64,
}

fn test_access_token(exp: u64, role: &'static str) -> String {
    test_access_token_with_issuer(exp, role, expected_token_issuer())
}

fn test_access_token_with_issuer(exp: u64, role: &'static str, iss: String) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("test-key".to_string());
    encode(
        &header,
        &TestClaims {
            sub: "user-1",
            role,
            aud: "authenticated",
            iss,
            exp,
        },
        &EncodingKey::from_ec_der(TEST_ES256_PRIVATE_KEY_DER),
    )
    .expect("test JWT should encode")
}

fn unique_test_root(suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("synth-thin-client-{suffix}-{nanos}"))
}

#[test]
fn startup_shared_handles_complete_without_reentrant_lock() {
    let root = tempfile::tempdir().unwrap();
    let config = AgentConfig {
        storage_root: root.path().to_string_lossy().into_owned(),
        port: 0,
        auto_start: false,
    };
    let (update_runtime, _commands) = UpdateRuntime::new();
    let telegram = Arc::new(telegram::Bridge::new(root.path().to_path_buf()));
    let state = Arc::new(Mutex::new(AppState {
        server_state: ServerState::Stopped,
        shutdown_tx: None,
        server_task: None,
        port: 0,
        config,
        started_at: Instant::now(),
        update_runtime: update_runtime.clone(),
        telegram: telegram.clone(),
    }));
    let worker_state = state.clone();
    let worker_root = root.path().to_path_buf();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let snapshot =
            make_http_runtime_state(&worker_state, worker_root, "synthetic-device".into(), 0);
        sender.send(snapshot).unwrap();
    });
    let snapshot = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("startup must not deadlock");
    worker.join().unwrap();
    assert!(Arc::ptr_eq(&snapshot.update_runtime, &update_runtime));
    assert!(Arc::ptr_eq(&snapshot.telegram, &telegram));
    assert!(
        state.try_lock().is_ok(),
        "construction releases the shared-state lock"
    );
}

#[test]
fn storage_device_fence_rejects_wrong_and_invalid_identity() {
    let mut headers = HeaderMap::new();
    assert!(require_expected_device(&headers, "device-a").is_ok());
    headers.insert("x-dendrite-device-id", HeaderValue::from_static("device-a"));
    assert!(require_expected_device(&headers, "device-a").is_ok());
    assert_eq!(
        require_expected_device(&headers, "device-b")
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    headers.insert(
        "x-dendrite-device-id",
        HeaderValue::from_bytes(&[0xff]).unwrap(),
    );
    assert!(require_expected_device(&headers, "device-a").is_err());
}

#[test]
fn normalize_storage_path_rejects_traversal() {
    assert!(normalize_storage_path("../bad.txt").is_err());
    assert!(normalize_storage_path("user/../../bad.txt").is_err());
    assert!(normalize_storage_path("C:/bad.txt").is_err());
}

#[test]
fn atomic_storage_write_never_clobbers_without_upsert() {
    let root = unique_test_root("atomic-no-clobber");
    std::fs::create_dir_all(&root).unwrap();
    let target = root.join("source.txt");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    runtime
        .block_on(write_storage_file_atomically(
            target.clone(),
            Bytes::from_static(b"first"),
            false,
        ))
        .expect("first write should commit");
    assert!(runtime
        .block_on(write_storage_file_atomically(
            target.clone(),
            Bytes::from_static(b"second"),
            false,
        ))
        .is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"first");

    runtime
        .block_on(write_storage_file_atomically(
            target.clone(),
            Bytes::from_static(b"replacement"),
            true,
        ))
        .expect("explicit upsert should replace");
    assert_eq!(std::fs::read(&target).unwrap(), b"replacement");
    let _ = std::fs::remove_dir_all(root);
}

#[cfg(unix)]
#[test]
fn storage_resolution_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let root = unique_test_root("symlink-root");
    let outside = unique_test_root("symlink-outside");
    std::fs::create_dir_all(root.join("uploads").join("user-1")).unwrap();
    std::fs::create_dir_all(root.join("outputs")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join("uploads").join("user-1").join("escape")).unwrap();

    assert!(
        resolve_storage_file(&root, StorageBucket::Uploads, "user-1/escape/secret.txt",).is_err()
    );
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[test]
fn storage_path_belongs_to_user_accepts_supported_prefixes() {
    assert!(storage_path_belongs_to_user("user-1/file.txt", "user-1"));
    assert!(storage_path_belongs_to_user(
        "personas/user-1/profile.json",
        "user-1"
    ));
    assert!(storage_path_belongs_to_user(
        "consolidated_features/user-1/style.json",
        "user-1"
    ));
    assert!(!storage_path_belongs_to_user("user-2/file.txt", "user-1"));
}

#[test]
fn collect_bucket_stats_counts_only_owned_files() {
    let root = unique_test_root("stats");
    let uploads_root = root.join("uploads");
    std::fs::create_dir_all(uploads_root.join("user-1")).unwrap();
    std::fs::create_dir_all(uploads_root.join("user-2")).unwrap();
    std::fs::create_dir_all(uploads_root.join("personas").join("user-1")).unwrap();

    std::fs::write(uploads_root.join("user-1").join("a.txt"), b"abc").unwrap();
    std::fs::write(uploads_root.join("user-2").join("b.txt"), b"abcd").unwrap();
    std::fs::write(
        uploads_root
            .join("personas")
            .join("user-1")
            .join("profile.json"),
        b"12345",
    )
    .unwrap();

    let stats = collect_bucket_stats(&uploads_root, "user-1");
    assert_eq!(stats.file_count, 2);
    assert_eq!(stats.total_size, 8);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn overview_counts_original_local_sources_on_this_device_only() {
    let sources = [("local", "device-a", "upload", 100), ("local", "device-a", "upload", 200),
        ("cloud", "device-a", "upload", 300), ("local", "device-b", "upload", 400),
        ("cloud", "device-a", "interview", 500)].map(|(mode, device, kind, size)| {
        serde_json::from_value::<SourceCatalogItem>(serde_json::json!({
            "id":"source", "persona_id":"persona", "persona_name":"Synthetic", "filename":"source.txt",
            "file_size":size, "status":"ready", "category":kind, "source_kind":kind,
            "source_usage":"persona_source_and_context", "storage_mode":mode,
            "storage_health":"present", "storage_device_id":device,
            "storage_transition_target":null, "storage_transition_status":null, "created_at":null
        })).unwrap()
    }).to_vec();
    let catalog = StoredSourceCatalog {
        user_id: "owner".into(),
        synced_at: String::new(),
        sources,
    };
    let stats = collect_local_source_stats(&catalog, "device-a");
    assert_eq!(stats.file_count, 2);
    assert_eq!(stats.total_size, 300);
    let empty = collect_local_source_stats(&catalog, "unpaired-device");
    assert_eq!(empty.file_count, 0);
    assert_eq!(empty.total_size, 0);
}

#[test]
fn local_jwks_verification_accepts_valid_authenticated_token() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let token = test_access_token(now + 300, "authenticated");
    let subject = decode_asymmetric_access_token(&token, Algorithm::ES256, &test_es256_jwk())
        .expect("valid token should verify locally");
    assert_eq!(subject, "user-1");
}

#[test]
fn local_jwks_verification_rejects_expired_or_wrong_role_tokens() {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let expired = decode_asymmetric_access_token(
        &test_access_token(now.saturating_sub(60), "authenticated"),
        Algorithm::ES256,
        &test_es256_jwk(),
    )
    .expect_err("expired token must fail");
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    assert_eq!(expired.auth_code, Some("auth_token_expired"));

    let wrong_role = decode_asymmetric_access_token(
        &test_access_token(now + 300, "anon"),
        Algorithm::ES256,
        &test_es256_jwk(),
    )
    .expect_err("non-authenticated role must fail");
    assert_eq!(wrong_role.auth_code, Some("auth_token_invalid"));
    let other_url = if RELEASE_CHANNEL == "preview" {
        SUPABASE_URL
    } else {
        "https://foreign-project.supabase.co"
    };
    let foreign_token =
        test_access_token_with_issuer(now + 300, "authenticated", format!("{other_url}/auth/v1"));
    let foreign =
        decode_asymmetric_access_token(&foreign_token, Algorithm::ES256, &test_es256_jwk())
            .expect_err("another environment's validly signed token must fail");
    assert_eq!(foreign.auth_code, Some("auth_token_invalid"));
}

#[test]
fn online_legacy_auth_distinguishes_outage_expiry_and_invalid_token() {
    let unavailable = online_auth_failure(StatusCode::TOO_MANY_REQUESTS, "rate limited");
    assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(unavailable.auth_code, Some("auth_provider_unavailable"));

    let expired = online_auth_failure(StatusCode::UNAUTHORIZED, "JWT expired");
    assert_eq!(expired.status, StatusCode::UNAUTHORIZED);
    assert_eq!(expired.auth_code, Some("auth_token_expired"));

    let invalid = online_auth_failure(StatusCode::UNAUTHORIZED, "invalid JWT");
    assert_eq!(invalid.status, StatusCode::UNAUTHORIZED);
    assert_eq!(invalid.auth_code, Some("auth_token_invalid"));
}

#[test]
fn auth_errors_expose_machine_readable_header_without_token_details() {
    let response = ApiError::auth_expired().into_response();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(AUTH_ERROR_HEADER),
        Some(&HeaderValue::from_static("auth_token_expired"))
    );
}

#[test]
fn web_app_handoff_matches_the_build_environment() {
    assert_eq!(web_app_url_for_project(SUPABASE_URL), PRODUCTION_WEB_APP);
    assert_eq!(
        web_app_url_for_project(configured_supabase_url()),
        WEB_APP_URL
    );
    assert_eq!(
        web_app_url_for_project("http://127.0.0.1:54321"),
        LOCAL_WEB_APP
    );
    assert_eq!(web_app_url_for_project("https://attacker.example"), "");
    assert!(STATUS_PAGE.contains("Add local source"));
    assert!(STATUS_PAGE.contains("Source files"));
    assert!(STATUS_PAGE.contains("Local service ready"));
    assert!(!STATUS_PAGE.contains("Connected"));
}

#[test]
fn source_help_uses_a_viewport_overlay_without_growing_the_source_list() {
    assert!(STATUS_PAGE.contains(".source-help-text{display:none;position:fixed"));
    assert!(STATUS_PAGE.contains(".source-help-text.visible{display:block}"));
    assert!(STATUS_PAGE.contains("function positionSourceHelp(wrap,help)"));
    assert!(STATUS_PAGE.contains("window.innerHeight-edge"));
    assert!(
        !STATUS_PAGE.contains(".source-help-text{display:none;position:absolute;right:0;top:100%")
    );
}

#[test]
fn files_handoff_allows_only_scoped_source_actions() {
    let base = configured_web_app_url();
    let source = Uuid::new_v4();
    let persona = Uuid::new_v4();
    for url in [
        base.to_string(),
        format!("{base}?upload=local"),
        format!("{base}?persona={persona}&migrate={source}"),
    ] {
        assert!(is_synthius_files_action(&url.parse().unwrap()), "{url}");
    }
    for url in [
        format!("{base}?upload=cloud"),
        format!("{base}?migrate=not-a-uuid"),
        format!("{base}?next=https://attacker.example"),
        "https://attacker.example/files".to_string(),
    ] {
        assert!(!is_synthius_files_action(&url.parse().unwrap()), "{url}");
    }
}

#[test]
fn source_catalog_rejects_unbounded_or_untrusted_display_data() {
    let valid = SourceCatalogPayload {
        generated_at: "2026-09-30T00:00:00Z".to_string(),
        sources: vec![SourceCatalogItem {
            id: Uuid::new_v4().to_string(),
            persona_id: Uuid::new_v4().to_string(),
            persona_name: "Synthetic Persona".to_string(),
            filename: "notes.txt".to_string(),
            file_size: 42,
            status: "ready".to_string(),
            category: "notes".to_string(),
            source_kind: "upload".to_string(),
            source_usage: "persona_source_and_context".to_string(),
            source_application_status: "queued".to_string(),
            source_applied_at: None,
            deletion_status: "active".to_string(),
            storage_mode: "local".to_string(),
            storage_path: Some("user-1/notes.txt".to_string()),
            storage_health: "present".to_string(),
            storage_device_id: Some(Uuid::new_v4().to_string()),
            storage_transition_target: None,
            storage_transition_status: None,
            created_at: Some("2026-09-30T00:00:00Z".to_string()),
        }],
    };
    assert!(validate_source_catalog(&valid).is_ok());
    let mut invalid = valid.clone();
    invalid.sources[0].filename = "bad\nname.txt".to_string();
    assert!(validate_source_catalog(&invalid).is_err());
    invalid = valid.clone();
    invalid.sources[0].storage_mode = "remote".to_string();
    assert!(validate_source_catalog(&invalid).is_err());
    for path in ["../secret.txt", "C:/secret.txt", "user-1/../../secret.txt"] {
        invalid = valid.clone();
        invalid.sources[0].storage_path = Some(path.to_string());
        assert!(validate_source_catalog(&invalid).is_err());
    }
}

#[test]
fn native_wiki_handoff_is_environment_and_path_fenced() {
    let base: tauri::Url = configured_web_app_url().parse().unwrap();
    let good = base.join("/wiki/memory/add-sources").unwrap();
    assert!(is_synthius_wiki_action(&good));
    for path in [
        "/wiki/memory/add-sources?next=https://attacker.example",
        "/wiki/memory/add-sources#fragment",
        "/settings/api-key",
    ] {
        assert!(!is_synthius_wiki_action(&base.join(path).unwrap()));
    }
    assert!(!is_synthius_wiki_action(
        &"https://attacker.example/wiki/memory/add-sources"
            .parse()
            .unwrap()
    ));
}

#[test]
fn source_folder_action_accepts_only_local_source_ids() {
    let source = Uuid::new_v4();
    let action = format!("http://127.0.0.1:1543/open-source-folder/{source}");
    assert_eq!(
        source_folder_action_id(&action.parse().unwrap(), 1543),
        Some(source)
    );
    for url in [
        format!("https://attacker.example/open-source-folder/{source}"),
        format!("http://127.0.0.1:1544/open-source-folder/{source}"),
        format!("http://localhost:1543/open-source-folder/{source}"),
        format!("{action}?path=C:/secret"),
        format!("{action}#fragment"),
        format!("http://user@127.0.0.1:1543/open-source-folder/{source}"),
        "http://127.0.0.1:1543/open-source-folder/../secret".to_string(),
        format!("{action}/extra"),
    ] {
        assert!(
            source_folder_action_id(&url.parse().unwrap(), 1543).is_none(),
            "{url}"
        );
    }
}

#[test]
fn source_eye_resolves_the_exact_file_parent_and_fails_closed() {
    let root = tempfile::tempdir().unwrap();
    ensure_storage_directories(root.path()).unwrap();
    let id = Uuid::new_v4();
    let folder = root.path().join("uploads/user-1/nested");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("real source.txt"), "synthetic source").unwrap();
    let mut catalog = json!({"user_id":"user-1","synced_at":"", "sources":[{
        "id":id.to_string(),"persona_id":Uuid::new_v4().to_string(),
        "persona_name":"Synthetic Persona","filename":"display name.txt","file_size":16,
        "status":"ready","category":"notes","source_kind":"upload",
        "source_usage":"context_only","storage_mode":"local","storage_health":"present",
        "storage_path":"user-1/nested/real source.txt"
    }]});
    let save = |value: &serde_json::Value| {
        std::fs::write(
            source_catalog_path(root.path()),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    };
    save(&catalog);
    assert_eq!(source_folder_for_id(root.path(), id).unwrap(), folder);
    assert!(source_folder_for_id(root.path(), Uuid::new_v4()).is_err());
    catalog["sources"][0]["storage_mode"] = json!("cloud");
    save(&catalog);
    assert!(source_folder_for_id(root.path(), id).is_err());
    catalog["sources"][0]["storage_mode"] = json!("local");
    for path in [
        "user-2/other.txt",
        "../secret.txt",
        "user-1/nested/missing.txt",
        "user-1/nested",
    ] {
        catalog["sources"][0]["storage_path"] = json!(path);
        save(&catalog);
        assert!(source_folder_for_id(root.path(), id).is_err(), "{path}");
    }
    catalog["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("storage_path");
    save(&catalog);
    // Older installations keep loading the catalogue while awaiting a fresh sync.
    assert!(serde_json::from_slice::<StoredSourceCatalog>(
        &std::fs::read(source_catalog_path(root.path())).unwrap()
    )
    .is_ok());
    assert!(source_folder_for_id(root.path(), id).is_err());
}

#[test]
fn status_webview_allows_only_its_local_page() {
    for path in ["/", "/status"] {
        assert!(allow_status_navigation(
            &format!("http://127.0.0.1:1543{path}").parse().unwrap(),
            1543
        ));
    }
    assert!(allow_status_navigation(
        &"http://127.0.0.1:1543/status?page_token=0123456789abcdef0123456789abcdef"
            .parse()
            .unwrap(),
        1543
    ));
    for url in [
        "http://127.0.0.1:1543/api/storage/stats",
        "http://127.0.0.1:1544/status",
        "http://localhost:1543/status",
        "https://attacker.example/status",
        "http://127.0.0.1:1543/status?redirect=https://attacker.example",
        "http://127.0.0.1:1543/status?page_token=short",
        "http://127.0.0.1:1543/status?page_token=0123456789abcdef0123456789abcdef&extra=1",
        "http://someone@127.0.0.1:1543/status",
        "file:///C:/secret.txt",
    ] {
        assert!(
            !allow_status_navigation(&url.parse().unwrap(), 1543),
            "{url}"
        );
    }
}

#[test]
fn local_files_action_is_fixed_and_cannot_open_arbitrary_paths() {
    assert!(is_storage_folder_action(
        &"http://127.0.0.1:1543/open-storage".parse().unwrap(),
        1543
    ));
    for url in [
        "https://attacker.example/open-storage",
        "http://127.0.0.1:1544/open-storage",
        "http://127.0.0.1:1543/open-storage?path=C:/",
        "file:///C:/",
        "http://someone@127.0.0.1:1543/open-storage",
        "http://127.0.0.1:1543/open-storage#path",
    ] {
        assert!(
            !is_storage_folder_action(&url.parse().unwrap(), 1543),
            "{url}"
        );
    }
}

#[test]
fn desktop_icons_have_native_sizes_and_rgba_pixels() {
    for (scale, expected) in [
        (1.0, 16),
        (1.25, 20),
        (1.5, 24),
        (2.0, 32),
        (2.5, 40),
        (3.0, 48),
        (4.0, 64),
    ] {
        let icon = desktop_icon(16.0 * scale);
        assert_eq!(icon.width(), expected);
        assert_eq!(icon.height(), expected);
        assert_eq!(icon.rgba().len(), (expected * expected * 4) as usize);
        assert!(icon.rgba().chunks(4).any(|pixel| pixel[3] > 220));
        assert!(icon
            .rgba()
            .chunks(4)
            .any(|pixel| pixel[3] > 0 && pixel[3] < 255));
        assert!(icon.rgba().chunks(4).any(|pixel| pixel[3] == 0));
    }
}

#[cfg(target_os = "windows")]
#[test]
fn windows_loads_both_monochrome_ico_variants_at_native_sizes() {
    let icons = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/tray");
    for variant in ["white", "black"] {
        for size in [16, 20, 24, 32, 40, 48, 64] {
            assert!(tray_icon::Icon::from_path(
                icons.join(format!("{variant}.ico")),
                Some((size, size))
            )
            .is_ok());
        }
    }
}

#[test]
fn real_loopback_http_storage_and_browser_connection_work() {
    let root = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        ensure_storage_directories(root.path()).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (update_runtime, _update_commands) = UpdateRuntime::new();
        let status_page_token = update_runtime.page_token().to_string();
        let state = HttpRuntimeState {
            uploads: Arc::new(uploads::UploadRuntime::new("synthetic-device")),
            source_relay: Arc::new(source_search::RelayRuntime::default()),
            storage_root: root.path().to_path_buf(), device_id: Uuid::new_v4().to_string(), port,
            started_at: Instant::now(), http_client: reqwest::Client::new(),
            jwks_cache: Arc::new(AsyncMutex::new(JwksCache {
                keys: Some(JwkSet { keys: vec![test_es256_jwk()] }),
                expires_at: Some(Instant::now() + Duration::from_secs(600)),
                ..JwksCache::default()
            })),
            update_runtime,
            telegram: Arc::new(telegram::Bridge::new(root.path().to_path_buf())),
        };
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            axum::serve(listener, build_http_router(state)).with_graceful_shutdown(async { let _ = stop_rx.await; }).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        let base = format!("http://127.0.0.1:{port}");
        let health = client.get(format!("{base}/api/health")).send().await.unwrap().json::<serde_json::Value>().await.unwrap();
        assert_eq!(health["status"], "healthy");
        let html = client.get(format!("{base}/status")).send().await.unwrap().text().await.unwrap();
        assert!(html.contains(configured_web_app_url()));
        assert!(!html.contains("__WEB_APP_URL__"));
        assert!(!html.contains("__UPDATE_TOKEN__"));
        assert!(!html.contains("__SOURCE_CATALOG__"));
        assert!(!html.contains("__CSP_NONCE__"));
        assert!(!html.contains(&status_page_token));
        assert!(html.contains("src=\"/brand.svg\""));
        assert!(html.contains("href=\"/open-storage\""));
        assert!(html.contains("id=\"update-button\""));
        let update_status = client.get(format!("{base}/api/update/status")).send().await.unwrap().json::<serde_json::Value>().await.unwrap();
        assert_eq!(update_status["phase"], "idle");
        let unauthorized_update = client.post(format!("{base}/api/update/install")).send().await.unwrap();
        assert_eq!(unauthorized_update.status(), StatusCode::FORBIDDEN);
        let brand = client.get(format!("{base}/brand.svg")).send().await.unwrap();
        assert_eq!(brand.headers().get("content-type").unwrap(), "image/svg+xml");
        assert!(brand.text().await.unwrap().contains("<svg"));
        let compiled_origin = tauri::Url::parse(WEB_APP_URL).unwrap().origin().ascii_serialization();
        let expected_origins = match RELEASE_CHANNEL {
            "local-dev" => vec!["http://localhost:3000", "http://127.0.0.1:3000"],
            "preview" => vec![compiled_origin.as_str(), "http://localhost:3000"],
            _ => vec!["https://app.synthius.ai", "http://localhost:3000"],
        };
        for origin in expected_origins {
            let preflight = client.request(reqwest::Method::OPTIONS, format!("{base}/api/storage/stats"))
                .header("origin", origin).header("access-control-request-method", "GET")
                .header("access-control-request-headers", "authorization").send().await.unwrap();
            assert_eq!(preflight.headers().get("access-control-allow-origin").unwrap(), origin);
            assert_eq!(preflight.headers().get(PRIVATE_NETWORK_HEADER).unwrap(), "true");
        }
        let disallowed = client.get(format!("{base}/api/health")).header("origin", "https://attacker.example").send().await.unwrap();
        assert!(disallowed.headers().get("access-control-allow-origin").is_none());
        let other_channel = if RELEASE_CHANNEL == "preview" { "https://app.synthius.ai" } else { "https://other-environment.example" };
        let cross_environment = client.get(format!("{base}/api/health")).header("origin", other_channel).send().await.unwrap();
        assert!(cross_environment.headers().get("access-control-allow-origin").is_none());
        if RELEASE_CHANNEL == "local-dev" {
            for origin in ["https://app.synthius.ai", "https://other-environment.example"] {
                let hosted = client.get(format!("{base}/api/health")).header("origin", origin).send().await.unwrap();
                assert!(hosted.headers().get("access-control-allow-origin").is_none());
            }
        }
        assert_eq!(health["release_channel"], RELEASE_CHANNEL);
        assert_eq!(health["supabase_url"], configured_supabase_url());
        let unauthenticated = client.get(format!("{base}/api/storage/stats")).send().await.unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let token = test_access_token(now + 300, "authenticated");
        let actual_device = health["device_id"].as_str().unwrap();
        let mismatch = client.post(format!("{base}/api/storage/upload/uploads/user-1/wrong.txt"))
            .bearer_auth(&token).header("x-dendrite-device-id", "wrong-device")
            .header("content-type", "multipart/form-data; boundary=fixture")
            .body("--fixture--\r\n").send().await.unwrap();
        assert_eq!(mismatch.status(), StatusCode::CONFLICT);
        assert_eq!(mismatch.headers().get("x-dendrite-device-id").unwrap(), actual_device);
        assert!(!root.path().join("uploads/user-1/wrong.txt").exists());
        let catalog_source_id = Uuid::new_v4().to_string();
        let catalog_persona_id = Uuid::new_v4().to_string();
        let catalog = json!({
            "generated_at": "2026-09-30T00:00:00Z",
            "sources": [{
                "id": catalog_source_id,
                "persona_id": catalog_persona_id,
                "persona_name": "Synthetic Persona",
                "filename": "notes.txt",
                "file_size": 42,
                "status": "ready",
                "category": "notes",
                "source_kind": "upload",
                "source_usage": "persona_source_and_context",
                "storage_mode": "local",
                "storage_path": "user-1/notes.txt",
                "storage_health": "present",
                "storage_device_id": null,
                "storage_transition_target": null,
                "storage_transition_status": null,
                "created_at": "2026-09-30T00:00:00Z"
            }]
        });
        let saved_catalog = client.post(format!("{base}/api/source-catalog"))
            .bearer_auth(&token).json(&catalog).send().await.unwrap();
        assert_eq!(saved_catalog.status(), StatusCode::OK);
        let stored_catalog: StoredSourceCatalog = serde_json::from_slice(
            &std::fs::read(source_catalog_path(root.path())).unwrap(),
        ).unwrap();
        assert_eq!(stored_catalog.user_id, "user-1");
        assert_eq!(stored_catalog.sources.len(), 1);
        assert_eq!(stored_catalog.sources[0].storage_path.as_deref(), Some("user-1/notes.txt"));
        let mut foreign_catalog = catalog.clone();
        foreign_catalog["sources"][0]["storage_path"] = json!("user-2/secret.txt");
        assert_eq!(client.post(format!("{base}/api/source-catalog")).bearer_auth(&token)
            .json(&foreign_catalog).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(client.get(format!("{base}/open-source-folder/{catalog_source_id}"))
            .send().await.unwrap().status(), StatusCode::NOT_FOUND, "folder actions never become HTTP launch endpoints");
        let private_catalog_url = format!("{base}/api/desktop/source-catalog");
        assert_eq!(client.post(&private_catalog_url).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        assert_eq!(client.post(&private_catalog_url).header("Origin", "https://attacker.example")
            .header("X-Dendrite-Update-Token", &status_page_token).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        let refreshed = client.post(&private_catalog_url).header("Origin", &base)
            .header("X-Dendrite-Update-Token", &status_page_token).send().await.unwrap();
        assert_eq!(refreshed.status(), StatusCode::OK);
        assert_eq!(refreshed.json::<serde_json::Value>().await.unwrap()["sources"][0]["id"], catalog_source_id);
        let direct_status = client.get(format!("{base}/status")).send().await.unwrap().text().await.unwrap();
        assert!(!direct_status.contains("Synthetic Persona"));
        assert!(!direct_status.contains(&status_page_token));
        let desktop_status = client
            .get(format!("{base}/status?page_token={status_page_token}"))
            .send().await.unwrap().text().await.unwrap();
        assert!(desktop_status.contains("Synthetic Persona"));
        assert!(desktop_status.contains(&status_page_token));
        let path = "user-1/source.txt";
        let body = "--fixture\r\nContent-Disposition: form-data; name=\"file\"; filename=\"source.txt\"\r\nContent-Type: text/plain\r\n\r\nSynthetic local source\r\n--fixture--\r\n";
        let upload = client.post(format!("{base}/api/storage/upload/uploads/{path}"))
            .bearer_auth(&token).header("content-type", "multipart/form-data; boundary=fixture").body(body).send().await.unwrap();
        assert_eq!(upload.status(), StatusCode::OK);
        let download = client.get(format!("{base}/api/storage/download/uploads/{path}")).bearer_auth(&token).send().await.unwrap();
        assert_eq!(download.text().await.unwrap(), "Synthetic local source");
        std::fs::create_dir_all(root.path().join("uploads/user-2")).unwrap();
        std::fs::write(root.path().join("uploads/user-2/other.txt"), b"other user's fixture").unwrap();
        let other = client.get(format!("{base}/api/storage/download/uploads/user-2/other.txt")).bearer_auth(&token).send().await.unwrap();
        assert_eq!(other.status(), StatusCode::FORBIDDEN);
        let stats = client.get(format!("{base}/api/storage/stats")).bearer_auth(&token).send().await.unwrap().json::<serde_json::Value>().await.unwrap();
        assert_eq!(stats["total_file_count"], 1);
        let _ = stop_tx.send(());
        server.await.unwrap();
    });
}
