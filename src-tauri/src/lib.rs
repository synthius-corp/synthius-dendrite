//! Synthius Dendrite local source runtime.
//!
//! The desktop helper no longer runs the full Python backend locally.
//! It provides a minimal localhost service for plaintext file storage,
//! deterministic technical parsing, health checks, and tray/menu affordances
//! while cloud APIs handle validated-corpus processing and model inference.

use synthius_dendrite_parser as parser;
mod source_search;
mod telegram;
mod updater;
mod uploads;
#[cfg(target_os = "windows")]
mod windows_icons;

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Match Supabase's documented public-JWKS cache horizon. Unknown signing keys
/// force an early refresh so zero-downtime key rotation remains supported.
const JWKS_CACHE_TTL: Duration = Duration::from_secs(600);
const JWKS_FORCE_REFRESH_COALESCE: Duration = Duration::from_secs(2);

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Multipart, Path as AxumPath, Query, State};
use axum::http::header::{
    HeaderName, AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, head, post};
use axum::{Json, Router};
use jsonwebtoken::errors::ErrorKind as JwtErrorKind;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use tauri::{
    async_runtime::JoinHandle,
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent, TrayIconId},
    Manager, Runtime, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio_util::io::ReaderStream;
use tower_http::cors::CorsLayer;
use uuid::Uuid;
use walkdir::WalkDir;

use updater::{run_update_schedule, run_update_worker, UpdateRuntime};

const SUPABASE_URL: &str = "https://wjegriwrnrwndbonxkeg.supabase.co";
include!(concat!(env!("OUT_DIR"), "/distribution.rs"));
const TRAY_ID: &str = "synthius-tray";
const PRIVATE_NETWORK_HEADER: &str = "access-control-allow-private-network";
const LOCAL_RUNTIME_HEADER: &str = "x-synth-local-runtime";
const AUTH_ERROR_HEADER: &str = "x-synth-auth-error";
const MAX_UPLOAD_BYTES: usize = 1024 * 1024 * 1024;
const DENDRITE_PROTOCOL_VERSION: u32 = 1;
const LOCAL_PARSER_TIMEOUT: Duration = Duration::from_secs(120);
const STATUS_PAGE: &str = include_str!("status.html");
const SOURCE_SELECTION: &str = include_str!("../../ui/source-selection.cjs");
const UPLOAD_PAGE_SCRIPT: &str = include_str!("../../ui/uploads.js");
const TELEGRAM_ERROR_SCRIPT: &str = include_str!("../../ui/telegram-errors.js");
const PRODUCTION_WEB_APP: &str = "https://app.synthius.ai/files";
const LOCAL_WEB_APP: &str = "http://localhost:3000/files";

fn web_app_url_for_project(project_url: &str) -> &'static str {
    if project_url.trim_end_matches('/') == configured_supabase_url().trim_end_matches('/') {
        return WEB_APP_URL;
    }
    match project_url.trim_end_matches('/') {
        "http://127.0.0.1:54321" | "http://localhost:54321" => LOCAL_WEB_APP,
        SUPABASE_URL => PRODUCTION_WEB_APP,
        _ => "",
    }
}

fn configured_web_app_url() -> &'static str {
    web_app_url_for_project(configured_supabase_url())
}

fn allow_status_navigation(url: &tauri::Url, port: u16) -> bool {
    let query_is_safe = url.query().is_none_or(|_| {
        let pairs = url.query_pairs().collect::<Vec<_>>();
        pairs.len() == 1
            && pairs[0].0 == "page_token"
            && pairs[0].1.len() == 32
            && pairs[0]
                .1
                .chars()
                .all(|character| character.is_ascii_hexdigit())
    });
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port() == Some(port)
        && matches!(url.path(), "/" | "/status")
        && query_is_safe
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn is_storage_folder_action(url: &tauri::Url, port: u16) -> bool {
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port() == Some(port)
        && url.path() == "/open-storage"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
}

fn source_folder_action_id(url: &tauri::Url, port: u16) -> Option<Uuid> {
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port() != Some(port)
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Uuid::parse_str(url.path().strip_prefix("/open-source-folder/")?).ok()
}

fn is_synthius_files_action(url: &tauri::Url) -> bool {
    let Ok(base) = configured_web_app_url().parse::<tauri::Url>() else {
        return false;
    };
    if url.scheme() != base.scheme()
        || url.host_str() != base.host_str()
        || url.port_or_known_default() != base.port_or_known_default()
        || url.path() != "/files"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    url.query_pairs().all(|(key, value)| match key.as_ref() {
        "upload" => value == "local",
        "telegram_import" => value == "1",
        "persona" | "migrate" => Uuid::parse_str(value.as_ref()).is_ok(),
        _ => false,
    })
}

fn is_synthius_wiki_action(url: &tauri::Url) -> bool {
    let Ok(base) = configured_web_app_url().parse::<tauri::Url>() else {
        return false;
    };
    url.scheme() == base.scheme()
        && url.host_str() == base.host_str()
        && url.port_or_known_default() == base.port_or_known_default()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(
            url.path(),
            "/wiki/memory/add-sources"
                | "/wiki/account/local-source-storage"
                | "/wiki/account/cloud-source-storage"
        )
}

fn desktop_icon(pixel_size: f64) -> Image<'static> {
    // Rendered individually from the SVG: avoid asking Windows to shrink a
    // 512px application image to a 16px notification-area glyph.
    let bytes: &[u8] = match pixel_size.round() as u32 {
        0..=16 => include_bytes!("../icons/tray/16x16.png"),
        17..=20 => include_bytes!("../icons/tray/20x20.png"),
        21..=24 => include_bytes!("../icons/tray/24x24.png"),
        25..=32 => include_bytes!("../icons/tray/32x32.png"),
        33..=40 => include_bytes!("../icons/tray/40x40.png"),
        41..=48 => include_bytes!("../icons/tray/48x48.png"),
        _ => include_bytes!("../icons/tray/64x64.png"),
    };
    Image::from_bytes(bytes).expect("valid generated Synthius desktop icon")
}

fn get_logs_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::home_dir()
            .map(|path| path.join("Library").join("Logs").join(DATA_DIRECTORY))
            .unwrap_or_else(|| PathBuf::from("./logs"))
    }

    #[cfg(target_os = "windows")]
    {
        dirs::data_local_dir()
            .map(|path| path.join(DATA_DIRECTORY).join("logs"))
            .unwrap_or_else(|| PathBuf::from("./logs"))
    }

    #[cfg(target_os = "linux")]
    {
        dirs::data_local_dir()
            .map(|path| path.join(DATA_DIRECTORY).join("logs"))
            .unwrap_or_else(|| PathBuf::from("./logs"))
    }
}

fn write_agent_log(message: &str) {
    let logs_dir = get_logs_dir();
    if let Err(error) = std::fs::create_dir_all(&logs_dir) {
        eprintln!("Failed to create logs directory: {error}");
        return;
    }

    let log_path = logs_dir.join("sidecar.log");
    if let Ok(metadata) = std::fs::metadata(&log_path) {
        if metadata.len() > 10 * 1024 * 1024 {
            let backup_path = logs_dir.join("sidecar.log.old");
            let _ = std::fs::rename(&log_path, &backup_path);
        }
    }

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(file, "[{timestamp}] {message}");
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerState {
    Stopped,
    Starting,
    Running,
    Error(String),
}

pub struct AppState {
    pub server_state: ServerState,
    pub shutdown_tx: Option<oneshot::Sender<()>>,
    pub server_task: Option<JoinHandle<()>>,
    pub port: u16,
    pub config: AgentConfig,
    pub started_at: Instant,
    pub update_runtime: Arc<UpdateRuntime>,
    pub telegram: Arc<telegram::Bridge>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentConfig {
    pub storage_root: String,
    pub port: u16,
    pub auto_start: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            storage_root: get_default_storage_root(),
            port: DEFAULT_PORT,
            auto_start: true,
        }
    }
}

#[derive(Clone)]
struct HttpRuntimeState {
    storage_root: PathBuf,
    device_id: String,
    port: u16,
    started_at: Instant,
    http_client: reqwest::Client,
    jwks_cache: Arc<AsyncMutex<JwksCache>>,
    update_runtime: Arc<UpdateRuntime>,
    telegram: Arc<telegram::Bridge>,
    source_relay: Arc<source_search::RelayRuntime>,
    uploads: Arc<uploads::UploadRuntime>,
}

#[derive(Default)]
struct JwksCache {
    keys: Option<JwkSet>,
    expires_at: Option<Instant>,
    refreshed_at: Option<Instant>,
    last_forced_at: Option<Instant>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    auth_code: Option<&'static str>,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            auth_code: None,
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
            auth_code: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            auth_code: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            auth_code: None,
        }
    }

    fn auth_required() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Authentication required".to_string(),
            auth_code: Some("auth_required"),
        }
    }

    fn auth_invalid() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Authentication token is invalid".to_string(),
            auth_code: Some("auth_token_invalid"),
        }
    }

    fn auth_expired() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Access token expired".to_string(),
            auth_code: Some("auth_token_expired"),
        }
    }

    fn auth_provider_unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "Authentication is temporarily unavailable".to_string(),
            auth_code: Some("auth_provider_unavailable"),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            auth_code: None,
        }
    }

    fn from_io(error: std::io::Error) -> Self {
        Self::internal(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(json!({
                "detail": if let Some(code) = self.auth_code {
                    json!({
                        "error": "authentication_failed",
                        "code": code,
                        "message": self.message,
                    })
                } else {
                    json!(self.message)
                },
            })),
        )
            .into_response();
        if let Some(code) = self.auth_code {
            response.headers_mut().insert(
                HeaderName::from_static(AUTH_ERROR_HEADER),
                HeaderValue::from_static(code),
            );
        }
        response
    }
}

#[derive(Clone, Copy)]
enum StorageBucket {
    Uploads,
    Outputs,
}

impl StorageBucket {
    fn parse(raw: &str) -> Result<Self, ApiError> {
        match raw {
            "uploads" => Ok(Self::Uploads),
            "outputs" => Ok(Self::Outputs),
            _ => Err(ApiError::bad_request(format!("Invalid bucket: {raw}"))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Uploads => "uploads",
            Self::Outputs => "outputs",
        }
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    upsert: Option<bool>,
}

#[derive(Deserialize)]
struct SupabaseUserResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct AccessTokenClaims {
    sub: String,
    role: String,
}

#[derive(Serialize)]
struct UploadResponse {
    status: &'static str,
    bucket: &'static str,
    path: String,
    size: usize,
    sha256: String,
    device_id: String,
}

#[derive(Deserialize)]
struct ParserClassifyRequest {
    storage_path: String,
    original_filename: String,
}

#[derive(Serialize)]
struct ParserClassifyResponse {
    category: String,
    subtype: String,
    confidence: f64,
    hints: Vec<String>,
    source_sha256: String,
    source_size: usize,
    device_id: String,
}

#[derive(Deserialize)]
struct ParserParseRequest {
    source_file_id: String,
    source_path: String,
    output_path: String,
    original_filename: String,
    category: String,
    subtype: String,
    subject_name: Option<String>,
}

#[derive(Serialize)]
struct ParserParseResponse {
    parser_id: String,
    subtype: String,
    parser_version: String,
    corpus_schema: String,
    corpus_path: String,
    corpus_sha256: String,
    corpus_size: usize,
    source_sha256: String,
    message_count: usize,
    word_count: usize,
    participants: Vec<parser::Participant>,
    date_start: Option<String>,
    date_end: Option<String>,
    device_id: String,
}

#[derive(Serialize)]
struct BucketStats {
    file_count: u64,
    total_size: u64,
}

#[derive(Serialize)]
struct StorageStatsResponse {
    uploads: BucketStats,
    outputs: BucketStats,
    total_file_count: u64,
    total_size: u64,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: String,
    build: String,
    uptime_seconds: f64,
    port: u16,
    storage_provider: &'static str,
    storage_root_ok: bool,
    db_connected: bool,
    supabase_project_id: Option<String>,
    supabase_url: &'static str,
    release_channel: &'static str,
    device_id: String,
    protocol_version: u32,
    platform: &'static str,
    product_name: &'static str,
    timestamp: String,
}

#[derive(Serialize)]
struct LocalOverviewResponse {
    status: &'static str,
    file_count: u64,
    total_size: u64,
    device_id: String,
    version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SourceCatalogItem {
    id: String,
    persona_id: String,
    persona_name: String,
    filename: String,
    file_size: u64,
    status: String,
    category: String,
    source_kind: String,
    source_usage: String,
    #[serde(default)]
    source_application_status: String,
    #[serde(default)]
    source_applied_at: Option<String>,
    #[serde(default)]
    deletion_status: String,
    storage_mode: String,
    #[serde(default)]
    storage_path: Option<String>,
    storage_health: String,
    storage_device_id: Option<String>,
    storage_transition_target: Option<String>,
    storage_transition_status: Option<String>,
    created_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SourceCatalogPayload {
    generated_at: String,
    sources: Vec<SourceCatalogItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredSourceCatalog {
    user_id: String,
    synced_at: String,
    sources: Vec<SourceCatalogItem>,
}

#[derive(Debug, Deserialize)]
struct StatusPageQuery {
    page_token: Option<String>,
}

fn platform_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "unsupported"
    }
}

fn get_device_id_path() -> PathBuf {
    // Preserve the established directory so a Dendrite upgrade never moves or
    // loses the existing local source vault.
    dirs::data_local_dir()
        .map(|path| path.join(DATA_DIRECTORY).join("device-id"))
        .unwrap_or_else(|| PathBuf::from("./device-id"))
}

fn load_or_create_device_id() -> std::io::Result<String> {
    let path = get_device_id_path();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let normalized = existing.trim();
        if Uuid::parse_str(normalized).is_ok() {
            return Ok(normalized.to_string());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let value = Uuid::new_v4().to_string();
    let temporary = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    std::fs::write(&temporary, format!("{value}\n"))?;
    match std::fs::rename(&temporary, &path) {
        Ok(()) => Ok(value),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            if let Ok(existing) = std::fs::read_to_string(&path) {
                let normalized = existing.trim();
                if Uuid::parse_str(normalized).is_ok() {
                    return Ok(normalized.to_string());
                }
            }
            Err(error)
        }
    }
}

fn get_default_storage_root() -> String {
    dirs::data_local_dir()
        .map(|path| path.join(DATA_DIRECTORY).join("storage"))
        .unwrap_or_else(|| PathBuf::from("./storage"))
        .to_string_lossy()
        .to_string()
}

fn get_default_config() -> AgentConfig {
    AgentConfig::default()
}

fn ensure_storage_directories(storage_root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(storage_root.join("uploads"))?;
    std::fs::create_dir_all(storage_root.join("outputs"))?;
    std::fs::create_dir_all(storage_root.join("metadata"))?;
    Ok(())
}

fn source_catalog_path(storage_root: &Path) -> PathBuf {
    storage_root.join("metadata").join("source-catalog.json")
}

fn validate_catalog_text(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && !value.chars().any(|character| character.is_control())
}

fn validate_source_catalog(catalog: &SourceCatalogPayload) -> Result<(), ApiError> {
    if catalog.sources.len() > 10_000 || catalog.generated_at.len() > 64 {
        return Err(ApiError::bad_request("Source catalog exceeds its limits"));
    }
    for source in &catalog.sources {
        if Uuid::parse_str(&source.id).is_err()
            || Uuid::parse_str(&source.persona_id).is_err()
            || !validate_catalog_text(source.persona_name.trim(), 160)
            || !validate_catalog_text(source.filename.trim(), 1024)
            || !validate_catalog_text(source.status.trim(), 64)
            || !validate_catalog_text(source.category.trim(), 80)
            || !validate_catalog_text(source.source_kind.trim(), 80)
            || !validate_catalog_text(source.source_usage.trim(), 80)
            || source.source_application_status.len() > 80
            || source
                .source_applied_at
                .as_deref()
                .is_some_and(|value| value.len() > 64)
            || source.deletion_status.len() > 32
            || !matches!(source.storage_mode.as_str(), "cloud" | "local")
            || source
                .storage_path
                .as_deref()
                .is_some_and(|path| path.len() > 4096 || normalize_storage_path(path).is_err())
            || source.storage_health.len() > 32
            || source
                .storage_device_id
                .as_deref()
                .is_some_and(|value| Uuid::parse_str(value).is_err())
            || source
                .storage_transition_target
                .as_deref()
                .is_some_and(|value| !matches!(value, "cloud" | "local"))
            || source
                .storage_transition_status
                .as_deref()
                .is_some_and(|value| value.len() > 32)
        {
            return Err(ApiError::bad_request(
                "Source catalog contains an invalid record",
            ));
        }
    }
    Ok(())
}

async fn read_source_catalog(storage_root: &Path) -> StoredSourceCatalog {
    let empty = StoredSourceCatalog {
        user_id: String::new(),
        synced_at: String::new(),
        sources: Vec::new(),
    };
    let Ok(bytes) = tokio::fs::read(source_catalog_path(storage_root)).await else {
        return empty;
    };
    serde_json::from_slice(&bytes).unwrap_or(empty)
}

fn source_folder_for_id(storage_root: &Path, source_id: Uuid) -> Result<PathBuf, ApiError> {
    let bytes = std::fs::read(source_catalog_path(storage_root)).map_err(ApiError::from_io)?;
    let catalog: StoredSourceCatalog = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::bad_request("Invalid source catalog"))?;
    let source = catalog
        .sources
        .iter()
        .find(|source| Uuid::parse_str(&source.id).ok() == Some(source_id))
        .ok_or_else(|| ApiError::not_found("Source is not in this account's catalog"))?;
    if source.storage_mode != "local" {
        return Err(ApiError::bad_request("Source is stored in Cloud"));
    }
    let path = source
        .storage_path
        .as_deref()
        .ok_or_else(|| ApiError::not_found("Refresh Sources in Synthius to locate this file"))?;
    if catalog.user_id.is_empty() || !storage_path_belongs_to_user(path, &catalog.user_id) {
        return Err(ApiError::forbidden(
            "Source path belongs to another account",
        ));
    }
    let (_, file) = resolve_storage_file(storage_root, StorageBucket::Uploads, path)?;
    if !file.is_file() {
        return Err(ApiError::not_found(
            "Local source file is unavailable on this computer",
        ));
    }
    file.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| ApiError::bad_request("Source file has no parent folder"))
}

fn normalize_storage_path(path: &str) -> Result<String, ApiError> {
    let mut segments = Vec::new();
    for segment in path.replace('\\', "/").split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." || segment.contains(':') || segment.contains('\0') {
            return Err(ApiError::bad_request("Invalid storage path"));
        }
        segments.push(segment.to_string());
    }

    if segments.is_empty() {
        return Err(ApiError::bad_request("Storage path cannot be empty"));
    }

    Ok(segments.join("/"))
}

fn extract_storage_owner_user_id(path: &str) -> Option<&str> {
    let mut segments = path.split('/');
    match segments.next()? {
        "personas" | "consolidated_features" => segments.next(),
        user_id => Some(user_id),
    }
}

fn storage_path_belongs_to_user(path: &str, user_id: &str) -> bool {
    normalize_storage_path(path)
        .ok()
        .and_then(|normalized| {
            extract_storage_owner_user_id(&normalized).map(|owner| owner == user_id)
        })
        .unwrap_or(false)
}

fn resolve_storage_file(
    storage_root: &Path,
    bucket: StorageBucket,
    path: &str,
) -> Result<(String, PathBuf), ApiError> {
    let normalized = normalize_storage_path(path)?;
    let bucket_root = storage_root.join(bucket.as_str());
    std::fs::create_dir_all(&bucket_root).map_err(ApiError::from_io)?;
    let canonical_root = std::fs::canonicalize(&bucket_root).map_err(ApiError::from_io)?;
    let candidate = bucket_root.join(PathBuf::from(&normalized));

    let mut current = bucket_root.clone();
    for segment in normalized.split('/') {
        current.push(segment);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(ApiError::forbidden(
                        "Symbolic links are not valid storage paths",
                    ));
                }
                let resolved = std::fs::canonicalize(&current).map_err(ApiError::from_io)?;
                if !resolved.starts_with(&canonical_root) {
                    return Err(ApiError::forbidden(
                        "Storage path leaves the configured root",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(ApiError::from_io(error)),
        }
    }

    Ok((normalized, candidate))
}

async fn write_storage_file_atomically(
    full_path: PathBuf,
    content: Bytes,
    upsert: bool,
) -> Result<(), ApiError> {
    let result = tokio::task::spawn_blocking(move || -> Result<(), std::io::Error> {
        let parent = full_path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Storage path has no parent",
            )
        })?;
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&content)?;
        temporary.as_file().sync_all()?;
        if upsert {
            temporary.persist(&full_path).map_err(|error| error.error)?;
        } else {
            temporary
                .persist_noclobber(&full_path)
                .map_err(|error| error.error)?;
        }
        Ok(())
    })
    .await
    .map_err(|error| ApiError::internal(format!("Storage write task failed: {error}")))?;

    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(ApiError::conflict("File already exists"))
        }
        Err(error) => Err(ApiError::from_io(error)),
    }
}

fn sanitize_filename_for_header(filename: &str) -> String {
    filename.replace('"', "_")
}

fn guess_content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("json") => "application/json",
        Some("jsonl") => "application/json",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        Some("csv") => "text/csv; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("ogg") => "audio/ogg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

fn collect_bucket_stats(bucket_root: &Path, user_id: &str) -> BucketStats {
    if !bucket_root.exists() {
        return BucketStats {
            file_count: 0,
            total_size: 0,
        };
    }

    let mut file_count = 0_u64;
    let mut total_size = 0_u64;

    for entry in WalkDir::new(bucket_root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }

        let Ok(relative) = entry.path().strip_prefix(bucket_root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !storage_path_belongs_to_user(&relative, user_id) {
            continue;
        }

        if let Ok(metadata) = entry.metadata() {
            file_count += 1;
            total_size += metadata.len();
        }
    }

    BucketStats {
        file_count,
        total_size,
    }
}

fn collect_local_source_stats(catalog: &StoredSourceCatalog, device_id: &str) -> BucketStats {
    let mut file_count = 0_u64;
    let mut total_size = 0_u64;
    for source in &catalog.sources {
        if source.storage_mode == "local"
            && source.storage_device_id.as_deref() == Some(device_id)
            && source.source_kind != "interview"
            && source.category != "interview"
        {
            file_count += 1;
            total_size = total_size.saturating_add(source.file_size);
        }
    }
    BucketStats {
        file_count,
        total_size,
    }
}

fn get_project_ref() -> Option<String> {
    configured_supabase_url()
        .strip_prefix("https://")
        .and_then(|url| url.split('.').next())
        .map(|project_ref| {
            let prefix = if project_ref.len() > 8 {
                &project_ref[..8]
            } else {
                project_ref
            };
            format!("{prefix}...")
        })
}

fn configured_supabase_url() -> &'static str {
    option_env!("SYNTHIUS_SUPABASE_URL").unwrap_or(SUPABASE_URL)
}

fn configured_supabase_public_key() -> &'static str {
    env!("SYNTHIUS_SUPABASE_PUBLIC_KEY")
}

fn expected_token_issuer() -> String {
    format!(
        "{}/auth/v1",
        configured_supabase_url().trim_end_matches('/')
    )
}

async fn get_project_jwks(
    state: &HttpRuntimeState,
    force_refresh: bool,
) -> Result<JwkSet, ApiError> {
    let now = Instant::now();
    let mut cache = state.jwks_cache.lock().await;

    if force_refresh {
        if let (Some(keys), Some(last_forced_at)) = (&cache.keys, cache.last_forced_at) {
            if now.duration_since(last_forced_at) < JWKS_FORCE_REFRESH_COALESCE {
                return Ok(keys.clone());
            }
        }
        cache.last_forced_at = Some(now);
    } else if let (Some(keys), Some(expires_at)) = (&cache.keys, cache.expires_at) {
        if expires_at > now {
            return Ok(keys.clone());
        }
    }

    let response = state
        .http_client
        .get(format!(
            "{}/auth/v1/.well-known/jwks.json",
            configured_supabase_url().trim_end_matches('/')
        ))
        .send()
        .await
        .map_err(|error| {
            write_agent_log(&format!(
                "Auth JWKS request failed: {}",
                error
                    .status()
                    .map(|status| status.as_u16().to_string())
                    .unwrap_or_else(|| "network".to_string())
            ));
            ApiError::auth_provider_unavailable()
        })?;

    if !response.status().is_success() {
        write_agent_log(&format!("Auth JWKS response status={}", response.status()));
        return Err(ApiError::auth_provider_unavailable());
    }

    let keys = response
        .json::<JwkSet>()
        .await
        .map_err(|_| ApiError::auth_provider_unavailable())?;
    if keys.keys.is_empty() {
        return Err(ApiError::auth_provider_unavailable());
    }

    cache.keys = Some(keys.clone());
    cache.expires_at = Some(Instant::now() + JWKS_CACHE_TTL);
    cache.refreshed_at = Some(Instant::now());
    Ok(keys)
}

fn decode_asymmetric_access_token(
    token: &str,
    algorithm: Algorithm,
    jwk: &jsonwebtoken::jwk::Jwk,
) -> Result<String, ApiError> {
    let key = DecodingKey::from_jwk(jwk).map_err(|_| ApiError::auth_invalid())?;
    let mut validation = Validation::new(algorithm);
    validation.leeway = 30;
    validation.validate_nbf = true;
    validation.set_required_spec_claims(&["exp", "sub", "aud", "iss", "role"]);
    validation.set_audience(&["authenticated"]);
    let issuer = expected_token_issuer();
    validation.set_issuer(&[issuer.as_str()]);

    match decode::<AccessTokenClaims>(token, &key, &validation) {
        Ok(data) if data.claims.role == "authenticated" && !data.claims.sub.trim().is_empty() => {
            Ok(data.claims.sub)
        }
        Ok(_) => Err(ApiError::auth_invalid()),
        Err(error) if matches!(error.kind(), JwtErrorKind::ExpiredSignature) => {
            Err(ApiError::auth_expired())
        }
        Err(_) => Err(ApiError::auth_invalid()),
    }
}

async fn verify_asymmetric_access_token(
    token: &str,
    algorithm: Algorithm,
    kid: &str,
    state: &HttpRuntimeState,
) -> Result<String, ApiError> {
    let keys = get_project_jwks(state, false).await?;
    if let Some(jwk) = keys.find(kid) {
        return decode_asymmetric_access_token(token, algorithm, jwk);
    }

    let refreshed_keys = get_project_jwks(state, true).await?;
    let jwk = refreshed_keys
        .find(kid)
        .ok_or_else(ApiError::auth_invalid)?;
    decode_asymmetric_access_token(token, algorithm, jwk)
}

fn online_auth_failure(status: StatusCode, body: &str) -> ApiError {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        return ApiError::auth_provider_unavailable();
    }
    if status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || status.is_client_error()
    {
        let normalized = body.to_ascii_lowercase();
        if normalized.contains("expired") {
            return ApiError::auth_expired();
        }
        return ApiError::auth_invalid();
    }
    ApiError::auth_provider_unavailable()
}

async fn verify_legacy_access_token(
    token: &str,
    state: &HttpRuntimeState,
) -> Result<String, ApiError> {
    let response = state
        .http_client
        .get(format!(
            "{}/auth/v1/user",
            configured_supabase_url().trim_end_matches('/')
        ))
        .header("apikey", configured_supabase_public_key())
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .map_err(|_| ApiError::auth_provider_unavailable())?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(online_auth_failure(status, &body));
    }

    let user = response
        .json::<SupabaseUserResponse>()
        .await
        .map_err(|_| ApiError::auth_provider_unavailable())?;
    if user.id.trim().is_empty() {
        return Err(ApiError::auth_invalid());
    }
    Ok(user.id)
}

fn require_expected_device(headers: &HeaderMap, actual_device_id: &str) -> Result<(), ApiError> {
    if let Some(expected) = headers.get("x-dendrite-device-id") {
        if expected.to_str().ok() != Some(actual_device_id) {
            return Err(ApiError::conflict(
                "This storage operation belongs to another Dendrite device",
            ));
        }
    }
    Ok(())
}

async fn authorize_request(
    headers: &HeaderMap,
    state: &HttpRuntimeState,
) -> Result<String, ApiError> {
    require_expected_device(headers, &state.device_id)?;
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(ApiError::auth_required)?;

    let header = decode_header(token).map_err(|_| ApiError::auth_invalid())?;
    match header.alg {
        Algorithm::ES256 | Algorithm::RS256 => {
            let kid = header.kid.as_deref().ok_or_else(ApiError::auth_invalid)?;
            verify_asymmetric_access_token(token, header.alg, kid, state).await
        }
        Algorithm::HS256 => verify_legacy_access_token(token, state).await,
        _ => Err(ApiError::auth_invalid()),
    }
}

async fn add_local_runtime_headers(request: axum::http::Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        HeaderName::from_static(PRIVATE_NETWORK_HEADER),
        HeaderValue::from_static("true"),
    );
    response.headers_mut().insert(
        HeaderName::from_static(LOCAL_RUNTIME_HEADER),
        HeaderValue::from_static("true"),
    );
    response
}

async fn health_handler(State(state): State<HttpRuntimeState>) -> Json<HealthResponse> {
    let storage_root_ok = ensure_storage_directories(&state.storage_root).is_ok();
    let status = if storage_root_ok {
        "healthy"
    } else {
        "degraded"
    };
    let build = option_env!("APP_BUILD")
        .map(str::to_string)
        .unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "release".to_string()
            }
        });

    Json(HealthResponse {
        status,
        version: env!("CARGO_PKG_VERSION").to_string(),
        build,
        uptime_seconds: (state.started_at.elapsed().as_secs_f64() * 100.0).round() / 100.0,
        port: state.port,
        storage_provider: "local_filesystem_plaintext",
        storage_root_ok,
        db_connected: true,
        supabase_project_id: get_project_ref(),
        supabase_url: configured_supabase_url(),
        release_channel: RELEASE_CHANNEL,
        device_id: state.device_id.clone(),
        protocol_version: DENDRITE_PROTOCOL_VERSION,
        platform: platform_name(),
        product_name: PRODUCT_NAME,
        timestamp: chrono::Utc::now().to_rfc3339(),
    })
}

async fn local_overview_handler(
    State(state): State<HttpRuntimeState>,
) -> Json<LocalOverviewResponse> {
    // The account-bound catalogue contains original sources, never corpus,
    // index or persona output artifacts. Scope the overview to this device.
    let catalog = read_source_catalog(&state.storage_root).await;
    let stats = collect_local_source_stats(&catalog, &state.device_id);
    Json(LocalOverviewResponse {
        status: "ready",
        file_count: stats.file_count,
        total_size: stats.total_size,
        device_id: state.device_id,
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

async fn status_page_handler(
    State(state): State<HttpRuntimeState>,
    Query(query): Query<StatusPageQuery>,
) -> Response {
    let nonce = Uuid::new_v4().simple().to_string();
    let trusted_desktop_page =
        query.page_token.as_deref() == Some(state.update_runtime.page_token());
    let exposed_page_token = if trusted_desktop_page {
        state.update_runtime.page_token()
    } else {
        ""
    };
    let token_json =
        serde_json::to_string(exposed_page_token).unwrap_or_else(|_| "\"\"".to_string());
    // File and persona names are visible only in the Tauri-owned status
    // window. Direct loopback visits get an empty catalogue; the per-process
    // page token is never placed in cloud metadata or logs.
    let catalog = if trusted_desktop_page {
        read_source_catalog(&state.storage_root).await
    } else {
        StoredSourceCatalog {
            user_id: String::new(),
            synced_at: String::new(),
            sources: Vec::new(),
        }
    };
    let catalog_json = serde_json::to_string(&catalog)
        .unwrap_or_else(|_| "{\"user_id\":\"\",\"synced_at\":\"\",\"sources\":[]}".to_string())
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    let html = STATUS_PAGE
        .replace("__PRODUCT_NAME__", PRODUCT_NAME)
        .replace("__WEB_APP_URL__", configured_web_app_url())
        .replace("__UPDATE_TOKEN__", &token_json)
        .replace("__SOURCE_CATALOG__", &catalog_json)
        .replace("__CSP_NONCE__", &nonce);
    let mut response = html.into_response();
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("cache-control"),
        HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&format!(
        "default-src 'self'; script-src 'nonce-{nonce}'; style-src 'unsafe-inline'; img-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    )) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("content-security-policy"), value);
    }
    response
}

async fn source_catalog_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(catalog): Json<SourceCatalogPayload>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    validate_source_catalog(&catalog)?;
    if catalog.sources.iter().any(|source| {
        source
            .storage_path
            .as_deref()
            .is_some_and(|path| !storage_path_belongs_to_user(path, &user_id))
    }) {
        return Err(ApiError::forbidden(
            "Source catalog path belongs to another account",
        ));
    }
    let stored = StoredSourceCatalog {
        user_id,
        synced_at: chrono::Utc::now().to_rfc3339(),
        sources: catalog.sources,
    };
    let bytes = serde_json::to_vec(&stored)
        .map_err(|_| ApiError::internal("Failed to serialize source catalog"))?;
    write_storage_file_atomically(
        source_catalog_path(&state.storage_root),
        Bytes::from(bytes),
        true,
    )
    .await?;
    Ok(Json(json!({
        "status": "saved",
        "source_count": stored.sources.len(),
        "synced_at": stored.synced_at,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRelayConnectRequest {
    ticket: String,
}

async fn source_relay_connect_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(body): Json<SourceRelayConnectRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    if body.ticket.is_empty() || body.ticket.len() > 4096 {
        return Err(ApiError::bad_request("Invalid source relay capability"));
    }
    state.source_relay.connect(
        state.storage_root.clone(),
        state.device_id.clone(),
        user_id,
        body.ticket,
    );
    Ok(Json(json!({"status":"connecting"})))
}

async fn update_status_handler(
    State(state): State<HttpRuntimeState>,
) -> Json<updater::UpdateSnapshot> {
    Json(state.update_runtime.snapshot())
}

async fn desktop_source_catalog_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
) -> Result<Json<StoredSourceCatalog>, ApiError> {
    // Only the trusted, process-bound desktop page receives this capability.
    // Browser-facing catalog writes remain owner-JWT authorized separately.
    if !state
        .update_runtime
        .authorize_page_action(&headers, state.port)
    {
        return Err(ApiError::forbidden(
            "Desktop source catalog was not authorized",
        ));
    }
    Ok(Json(read_source_catalog(&state.storage_root).await))
}

fn authorize_update_action(headers: &HeaderMap, state: &HttpRuntimeState) -> Result<(), ApiError> {
    if state
        .update_runtime
        .authorize_page_action(headers, state.port)
    {
        Ok(())
    } else {
        Err(ApiError::forbidden("Update action was not authorized"))
    }
}

async fn update_install_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    authorize_update_action(&headers, &state)?;
    state
        .update_runtime
        .request_install()
        .map_err(ApiError::conflict)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "status": "downloading" })),
    ))
}

async fn brand_handler() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "image/svg+xml")],
        include_str!("../icons/brand.svg"),
    )
}

async fn storage_stats_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
) -> Result<Json<StorageStatsResponse>, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    let uploads = collect_bucket_stats(&state.storage_root.join("uploads"), &user_id);
    let outputs = collect_bucket_stats(&state.storage_root.join("outputs"), &user_id);

    Ok(Json(StorageStatsResponse {
        total_file_count: uploads.file_count + outputs.file_count,
        total_size: uploads.total_size + outputs.total_size,
        uploads,
        outputs,
    }))
}

async fn head_download_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    AxumPath((bucket_raw, path)): AxumPath<(String, String)>,
) -> Result<Response, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    let bucket = StorageBucket::parse(&bucket_raw)?;
    let (normalized, full_path) = resolve_storage_file(&state.storage_root, bucket, &path)?;

    if !storage_path_belongs_to_user(&normalized, &user_id) {
        return Err(ApiError::forbidden(
            "You do not have permission to access this file",
        ));
    }

    let metadata = tokio::fs::metadata(&full_path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ApiError::not_found("File not found"),
            _ => ApiError::from_io(error),
        })?;

    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static(guess_content_type(&full_path)),
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&metadata.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    Ok(response)
}

async fn download_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    AxumPath((bucket_raw, path)): AxumPath<(String, String)>,
) -> Result<Response, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    let bucket = StorageBucket::parse(&bucket_raw)?;
    let (normalized, full_path) = resolve_storage_file(&state.storage_root, bucket, &path)?;

    if !storage_path_belongs_to_user(&normalized, &user_id) {
        return Err(ApiError::forbidden(
            "You do not have permission to access this file",
        ));
    }

    let file = tokio::fs::File::open(&full_path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ApiError::not_found("File not found"),
            _ => ApiError::from_io(error),
        })?;
    let metadata = file.metadata().await.map_err(ApiError::from_io)?;
    let filename = full_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("download.bin");

    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static(guess_content_type(&full_path)),
    );
    response.headers_mut().insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&metadata.len().to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );
    if let Ok(header_value) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"",
        sanitize_filename_for_header(filename)
    )) {
        response
            .headers_mut()
            .insert(CONTENT_DISPOSITION, header_value);
    }

    Ok(response)
}

async fn upload_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    AxumPath((bucket_raw, path)): AxumPath<(String, String)>,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    let bucket = StorageBucket::parse(&bucket_raw)?;
    let (normalized, full_path) = resolve_storage_file(&state.storage_root, bucket, &path)?;

    if !storage_path_belongs_to_user(&normalized, &user_id) {
        return Err(ApiError::forbidden(
            "You do not have permission to upload to this path",
        ));
    }

    let mut file_content = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::bad_request(error.to_string()))?
    {
        if field.name() == Some("file") {
            file_content = Some(
                field
                    .bytes()
                    .await
                    .map_err(|error| ApiError::bad_request(error.to_string()))?,
            );
            break;
        }
    }

    let content = file_content.ok_or_else(|| ApiError::bad_request("Missing file field"))?;
    let content_length = content.len();
    let content_sha256 = format!("{:x}", Sha256::digest(&content));
    write_storage_file_atomically(full_path, content, query.upsert.unwrap_or(false)).await?;

    Ok(Json(UploadResponse {
        status: "uploaded",
        bucket: bucket.as_str(),
        path: normalized,
        size: content_length,
        sha256: content_sha256,
        device_id: state.device_id,
    }))
}

async fn read_bounded_local_source(path: PathBuf) -> Result<Vec<u8>, ApiError> {
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ApiError::not_found("Local source not found"),
            _ => ApiError::from_io(error),
        })?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 * 1024 {
        return Err(ApiError::bad_request(
            "Local source exceeds the parser limit",
        ));
    }
    tokio::fs::read(path).await.map_err(ApiError::from_io)
}

async fn parser_classify_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(request): Json<ParserClassifyRequest>,
) -> Result<Json<ParserClassifyResponse>, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    classify_source_for_owner(state, &user_id, request).await
}

async fn classify_source_for_owner(
    state: HttpRuntimeState,
    user_id: &str,
    request: ParserClassifyRequest,
) -> Result<Json<ParserClassifyResponse>, ApiError> {
    let (storage_path, full_path) = resolve_storage_file(
        &state.storage_root,
        StorageBucket::Uploads,
        &request.storage_path,
    )?;
    if !storage_path_belongs_to_user(&storage_path, user_id) {
        return Err(ApiError::forbidden(
            "You do not have permission to classify this source",
        ));
    }
    let content = read_bounded_local_source(full_path).await?;
    classify_local_content(request.original_filename, content, state.device_id).await
}

async fn classify_local_content(
    filename: String,
    content: Vec<u8>,
    device_id: String,
) -> Result<Json<ParserClassifyResponse>, ApiError> {
    let source_sha256 = format!("{:x}", Sha256::digest(&content));
    let source_size = content.len();
    let classification = tokio::time::timeout(
        LOCAL_PARSER_TIMEOUT,
        tokio::task::spawn_blocking(move || parser::classify(&filename, &content)),
    )
    .await
    .map_err(|_| ApiError::bad_request("Local parser exceeded its time limit"))?
    .map_err(|_| ApiError::internal("Local parser task failed"))?
    .map_err(ApiError::bad_request)?;
    Ok(Json(ParserClassifyResponse {
        category: classification.category,
        subtype: classification.subtype,
        confidence: classification.confidence,
        hints: classification.hints,
        source_sha256,
        source_size,
        device_id,
    }))
}

fn validate_parser_paths(user_id: &str, request: &ParserParseRequest) -> Result<(), ApiError> {
    Uuid::parse_str(&request.source_file_id)
        .map_err(|_| ApiError::bad_request("Invalid source identifier"))?;
    let expected_source_prefix = format!("{}/{}/", user_id, request.source_file_id);
    let expected_output = format!("{}/corpus/{}/corpus.jsonl", user_id, request.source_file_id);
    let source_path = normalize_storage_path(&request.source_path)?;
    let output_path = normalize_storage_path(&request.output_path)?;
    if !source_path.starts_with(&expected_source_prefix) || output_path != expected_output {
        return Err(ApiError::forbidden(
            "Local parser path does not match the source identity",
        ));
    }
    Ok(())
}

async fn parser_parse_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(request): Json<ParserParseRequest>,
) -> Result<Json<ParserParseResponse>, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    parse_source_for_owner(state, &user_id, request).await
}

async fn parse_source_for_owner(
    state: HttpRuntimeState,
    user_id: &str,
    request: ParserParseRequest,
) -> Result<Json<ParserParseResponse>, ApiError> {
    validate_parser_paths(user_id, &request)?;
    let (source_path, source_full_path) = resolve_storage_file(
        &state.storage_root,
        StorageBucket::Uploads,
        &request.source_path,
    )?;
    let (output_path, output_full_path) = resolve_storage_file(
        &state.storage_root,
        StorageBucket::Uploads,
        &request.output_path,
    )?;
    if !storage_path_belongs_to_user(&source_path, user_id)
        || !storage_path_belongs_to_user(&output_path, user_id)
    {
        return Err(ApiError::forbidden(
            "You do not have permission to parse this source",
        ));
    }
    let content = read_bounded_local_source(source_full_path).await?;
    let filename = request.original_filename;
    let category = request.category;
    let subtype = request.subtype;
    let subject_name = request.subject_name.unwrap_or_default();
    let parsed = tokio::time::timeout(
        LOCAL_PARSER_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            parser::parse(&filename, &content, &category, &subtype, &subject_name)
        }),
    )
    .await
    .map_err(|_| ApiError::bad_request("Local parser exceeded its time limit"))?
    .map_err(|_| ApiError::internal("Local parser task failed"))?
    .map_err(ApiError::bad_request)?;

    if let Ok(existing) = tokio::fs::read(&output_full_path).await {
        if format!("{:x}", Sha256::digest(&existing)) != parsed.corpus_sha256 {
            return Err(ApiError::conflict(
                "A different parsed corpus already exists",
            ));
        }
    } else {
        write_storage_file_atomically(output_full_path, Bytes::from(parsed.corpus.clone()), false)
            .await?;
    }

    Ok(Json(ParserParseResponse {
        parser_id: parsed.parser_id,
        subtype: parsed.subtype,
        parser_version: parsed.parser_version,
        corpus_schema: parsed.corpus_schema,
        corpus_path: output_path,
        corpus_sha256: parsed.corpus_sha256,
        corpus_size: parsed.corpus.len(),
        source_sha256: parsed.source_sha256,
        message_count: parsed.message_count,
        word_count: parsed.word_count,
        participants: parsed.participants,
        date_start: parsed.date_start,
        date_end: parsed.date_end,
        device_id: state.device_id,
    }))
}

async fn delete_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    AxumPath((bucket_raw, path)): AxumPath<(String, String)>,
) -> Result<Json<HashMap<&'static str, String>>, ApiError> {
    let user_id = authorize_request(&headers, &state).await?;
    let bucket = StorageBucket::parse(&bucket_raw)?;
    let (normalized, full_path) = resolve_storage_file(&state.storage_root, bucket, &path)?;

    if !storage_path_belongs_to_user(&normalized, &user_id) {
        return Err(ApiError::forbidden(
            "You do not have permission to delete this file",
        ));
    }

    tokio::fs::remove_file(&full_path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => ApiError::not_found("File not found"),
            _ => ApiError::from_io(error),
        })?;

    let mut response = HashMap::new();
    response.insert("status", "deleted".to_string());
    response.insert("bucket", bucket.as_str().to_string());
    response.insert("path", normalized);
    Ok(Json(response))
}

async fn add_device_identity(
    State(state): State<HttpRuntimeState>,
    request: axum::http::Request<Body>,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&state.device_id) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("x-dendrite-device-id"), value);
    }
    response
}

fn build_http_router(state: HttpRuntimeState) -> Router {
    let mut allowed_origins = if RELEASE_CHANNEL == "local-dev" {
        Vec::new()
    } else {
        let origin = tauri::Url::parse(WEB_APP_URL)
            .expect("Compiled webapp URL")
            .origin()
            .ascii_serialization();
        vec![HeaderValue::from_str(&origin).expect("Compiled webapp origin")]
    };
    if cfg!(debug_assertions) {
        allowed_origins.extend([
            HeaderValue::from_static("http://localhost:3000"),
            HeaderValue::from_static("http://127.0.0.1:3000"),
        ]);
    }
    Router::new()
        .route("/", get(status_page_handler))
        .route("/status", get(status_page_handler))
        .route("/brand.svg", get(brand_handler))
        .route(
            "/source-selection.js",
            get(|| async {
                (
                    [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    SOURCE_SELECTION,
                )
            }),
        )
        .route(
            "/uploads.js",
            get(|| async {
                (
                    [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    UPLOAD_PAGE_SCRIPT,
                )
            }),
        )
        .route(
            "/telegram-errors.js",
            get(|| async {
                (
                    [(CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    TELEGRAM_ERROR_SCRIPT,
                )
            }),
        )
        .route("/api/status", get(local_overview_handler))
        .route("/api/health", get(health_handler))
        .route("/api/update/status", get(update_status_handler))
        .route("/api/update/install", post(update_install_handler))
        .route("/api/source-catalog", post(source_catalog_handler))
        .route(
            "/api/source-relay/connect",
            post(source_relay_connect_handler),
        )
        .route("/api/uploads/connect", post(uploads::connect_handler))
        .route(
            "/api/desktop/uploads/context",
            post(uploads::context_handler),
        )
        .route(
            "/api/desktop/telegram/open",
            post(uploads::telegram_open_handler),
        )
        .route(
            "/api/desktop/telegram/register",
            post(uploads::telegram_register_handler),
        )
        .route(
            "/api/desktop/uploads/add",
            post(uploads::add_handler).layer(DefaultBodyLimit::max(51 * 1024 * 1024)),
        )
        .route("/api/desktop/uploads/parse", post(uploads::parse_handler))
        .route(
            "/api/desktop/uploads/source-action",
            post(uploads::source_action_handler),
        )
        .route(
            "/api/desktop/source-catalog",
            post(desktop_source_catalog_handler),
        )
        .route("/api/storage/stats", get(storage_stats_handler))
        .route(
            "/api/storage/download/:bucket/*path",
            get(download_handler).route_layer(middleware::from_fn(add_local_runtime_headers)),
        )
        .route(
            "/api/storage/download/:bucket/*path",
            head(head_download_handler).route_layer(middleware::from_fn(add_local_runtime_headers)),
        )
        .route("/api/storage/upload/:bucket/*path", post(upload_handler))
        .route("/api/storage/delete/:bucket/*path", delete(delete_handler))
        .route("/api/parser/classify", post(parser_classify_handler))
        .route("/api/parser/parse", post(parser_parse_handler))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            add_device_identity,
        ))
        .with_state(state)
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
        .layer(
            CorsLayer::new()
                .allow_origin(allowed_origins)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::DELETE,
                    Method::HEAD,
                    Method::OPTIONS,
                ])
                .allow_headers([
                    AUTHORIZATION,
                    CONTENT_TYPE,
                    HeaderName::from_static("access-control-request-private-network"),
                    HeaderName::from_static("x-dendrite-device-id"),
                ])
                .expose_headers([
                    HeaderName::from_static("x-dendrite-device-id"),
                    HeaderName::from_static(AUTH_ERROR_HEADER),
                    HeaderName::from_static(LOCAL_RUNTIME_HEADER),
                    CONTENT_DISPOSITION,
                    CONTENT_LENGTH,
                ]),
        )
        .layer(middleware::from_fn(add_local_runtime_headers))
}

fn make_http_runtime_state(
    state: &Arc<Mutex<AppState>>,
    storage_root: PathBuf,
    device_id: String,
    port: u16,
) -> HttpRuntimeState {
    // Snapshot shared handles under one guard. Temporary MutexGuards inside a
    // struct literal otherwise live until the whole statement ends.
    let (update_runtime, telegram) = {
        let state_guard = state.lock().unwrap();
        (
            state_guard.update_runtime.clone(),
            state_guard.telegram.clone(),
        )
    };
    HttpRuntimeState {
        uploads: Arc::new(uploads::UploadRuntime::new(&device_id)),
        storage_root,
        device_id,
        port,
        started_at: Instant::now(),
        http_client: reqwest::Client::new(),
        jwks_cache: Arc::new(AsyncMutex::new(JwksCache::default())),
        update_runtime,
        telegram,
        source_relay: Arc::new(source_search::RelayRuntime::default()),
    }
}

async fn start_backend<R: Runtime>(
    app: &tauri::AppHandle<R>,
    state: Arc<Mutex<AppState>>,
) -> Result<(), String> {
    write_agent_log("=== Starting local runtime ===");
    write_agent_log(&format!(
        "Platform: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    write_agent_log(&format!("App version: {}", env!("CARGO_PKG_VERSION")));

    let config = {
        let mut state_guard = state.lock().unwrap();
        if state_guard.server_state == ServerState::Running
            || state_guard.server_state == ServerState::Starting
        {
            write_agent_log("Local runtime already running or starting, skipping");
            return Ok(());
        }

        state_guard.server_state = ServerState::Starting;
        state_guard.config.clone()
    };

    let port = config.port;
    let storage_root = PathBuf::from(&config.storage_root);
    let device_id = load_or_create_device_id().map_err(|error| {
        let message = format!("Failed to initialize device identity: {error}");
        write_agent_log(&format!("ERROR: {message}"));
        message
    })?;
    write_agent_log(&format!("Port: {port}"));
    write_agent_log(&format!("Storage root: {}", storage_root.display()));

    ensure_storage_directories(&storage_root).map_err(|error| {
        let message = format!("Failed to prepare local storage: {error}");
        write_agent_log(&format!("ERROR: {message}"));
        message
    })?;

    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| {
            let message = format!("Failed to bind local runtime on port {port}: {error}");
            write_agent_log(&format!("ERROR: {message}"));
            message
        })?;

    let http_state = make_http_runtime_state(&state, storage_root.clone(), device_id, port);
    let router = build_http_router(http_state.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let app_handle = app.clone();
    let state_for_task = state.clone();
    let storage_root_for_task = config.storage_root.clone();

    let server_task = tauri::async_runtime::spawn(async move {
        let relay_restore = tokio::spawn(async move {
            uploads::restore_source_relay(&http_state).await;
        });
        write_agent_log(&format!(
            "Local runtime listening on http://127.0.0.1:{DEFAULT_PORT}"
        ));
        let result = axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;

        relay_restore.abort();

        if let Err(error) = result {
            let message = format!("Local runtime stopped unexpectedly: {error}");
            write_agent_log(&format!("ERROR: {message}"));
            let next_state = {
                let mut state_guard = state_for_task.lock().unwrap();
                match state_guard.server_state {
                    ServerState::Stopped => None,
                    _ => {
                        state_guard.server_state = ServerState::Error(message.clone());
                        Some(state_guard.server_state.clone())
                    }
                }
            };

            if let Some(server_state) = next_state {
                refresh_tray_menu(&app_handle, &server_state, &storage_root_for_task);
            }
        }
    });

    {
        let mut state_guard = state.lock().unwrap();
        state_guard.shutdown_tx = Some(shutdown_tx);
        state_guard.server_task = Some(server_task);
        state_guard.port = port;
        state_guard.started_at = Instant::now();
        state_guard.server_state = ServerState::Running;
    }

    refresh_tray_menu(app, &ServerState::Running, &config.storage_root);
    Ok(())
}

fn stop_backend(state: Arc<Mutex<AppState>>) {
    let (shutdown_tx, server_task) = {
        let mut state_guard = state.lock().unwrap();
        let shutdown_tx = state_guard.shutdown_tx.take();
        let server_task = state_guard.server_task.take();
        state_guard.server_state = ServerState::Stopped;
        (shutdown_tx, server_task)
    };

    if let Some(tx) = shutdown_tx {
        let _ = tx.send(());
    }
    if let Some(task) = server_task {
        task.abort();
    }

    write_agent_log("Local runtime stopped");
}

fn refresh_tray_menu<R: Runtime>(
    app: &tauri::AppHandle<R>,
    server_state: &ServerState,
    storage_root: &str,
) {
    if let Some(tray) = app.tray_by_id(&TrayIconId::new(TRAY_ID)) {
        let storage_size = get_storage_size(storage_root);
        let new_menu = build_tray_menu(app, server_state, storage_size);
        let _ = tray.set_menu(Some(new_menu));
        log::info!("Tray menu refreshed with state: {server_state:?}");
    }
}

fn show_status_window<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("dendrite") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let (folder, page_token) = {
        let state = app.state::<Arc<Mutex<AppState>>>();
        let state = state.lock().unwrap();
        (
            state.config.storage_root.clone(),
            state.update_runtime.page_token().to_string(),
        )
    };
    let url = format!("http://127.0.0.1:{DEFAULT_PORT}/status?page_token={page_token}")
        .parse()
        .expect("valid local Dendrite status URL");
    let scale = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| monitor.scale_factor())
        .unwrap_or(1.0);
    let window = WebviewWindowBuilder::new(app, "dendrite", WebviewUrl::External(url))
        .disable_drag_drop_handler()
        .title(PRODUCT_NAME)
        .icon(desktop_icon(16.0 * scale))
        .expect("valid window icon")
        .inner_size(920.0, 680.0)
        .min_inner_size(760.0, 560.0)
        .resizable(true)
        .on_navigation(move |url| {
            if let Some(source_id) = source_folder_action_id(url, DEFAULT_PORT) {
                // Native window navigation only, never an HTTP launch endpoint.
                // Resolve the source ID against the owner's cached catalogue;
                // no URL-controlled filesystem path or program is accepted.
                match source_folder_for_id(Path::new(&folder), source_id) {
                    Ok(source_folder) => {
                        if open::that(source_folder).is_err() {
                            write_agent_log("Could not open the source file's folder");
                        }
                    }
                    Err(_) => write_agent_log("Local source folder is unavailable"),
                }
                return false;
            }
            if is_storage_folder_action(url, DEFAULT_PORT) {
                // Fixed configured folder only: no page-controlled path or
                // unauthenticated HTTP endpoint capable of launching programs.
                if open::that(&folder).is_err() {
                    write_agent_log("Could not open the local source folder");
                }
                return false;
            }
            if is_synthius_files_action(url) || is_synthius_wiki_action(url) {
                if open::that(url.as_str()).is_err() {
                    write_agent_log("Could not open Synthius in the default browser");
                }
                return false;
            }
            allow_status_navigation(url, DEFAULT_PORT)
        })
        .build();
    #[cfg(target_os = "windows")]
    if let Ok(window) = window {
        windows_icons::apply_window(&window);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = window;
}

fn build_tray_menu<R: Runtime>(
    app: &tauri::AppHandle<R>,
    state: &ServerState,
    storage_size: Option<u64>,
) -> Menu<R> {
    let menu = Menu::new(app).unwrap();

    let version_text = format!("{PRODUCT_NAME} v{}", env!("CARGO_PKG_VERSION"));
    let version_item = MenuItem::new(app, &version_text, false, None::<&str>).unwrap();
    let _ = menu.append(&version_item);

    let separator1 = MenuItem::new(app, "─────────────", false, None::<&str>).unwrap();
    let _ = menu.append(&separator1);

    let status_text = match state {
        ServerState::Stopped => "⏹ Local Runtime: Stopped".to_string(),
        ServerState::Starting => "⏳ Local Runtime: Starting...".to_string(),
        ServerState::Running => "✓ Local Runtime: Running".to_string(),
        ServerState::Error(error) => format!("✗ Runtime Error: {error}"),
    };
    let status_item = MenuItem::new(app, &status_text, false, None::<&str>).unwrap();
    let _ = menu.append(&status_item);

    let open_status_item =
        MenuItem::with_id(app, "open_status", "Open Dendrite", true, None::<&str>).unwrap();
    let _ = menu.append(&open_status_item);

    let open_web_item =
        MenuItem::with_id(app, "open_web", "Open Synthius web app", true, None::<&str>).unwrap();
    let _ = menu.append(&open_web_item);

    let storage_text = match storage_size {
        Some(size) => format!("📁 Local Storage: {}", format_bytes(size)),
        None => "📁 Local Storage: --".to_string(),
    };
    let storage_item =
        MenuItem::with_id(app, "open_storage", &storage_text, true, None::<&str>).unwrap();
    let _ = menu.append(&storage_item);

    let separator2 = MenuItem::new(app, "─────────────", false, None::<&str>).unwrap();
    let _ = menu.append(&separator2);

    let open_logs_item =
        MenuItem::with_id(app, "open_logs", "📋 Open Logs Folder", true, None::<&str>).unwrap();
    let _ = menu.append(&open_logs_item);

    let separator3 = MenuItem::new(app, "─────────────", false, None::<&str>).unwrap();
    let _ = menu.append(&separator3);

    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>).unwrap();
    let _ = menu.append(&quit_item);

    menu
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn get_storage_size(storage_root: &str) -> Option<u64> {
    let path = Path::new(storage_root);
    if !path.exists() {
        return Some(0);
    }

    fn dir_size(path: &Path) -> u64 {
        let mut total = 0;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                if entry_path.is_file() {
                    if let Ok(metadata) = entry.metadata() {
                        total += metadata.len();
                    }
                } else if entry_path.is_dir() {
                    total += dir_size(&entry_path);
                }
            }
        }
        total
    }

    Some(dir_size(path))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config = get_default_config();
    let telegram_bridge = Arc::new(telegram::Bridge::new(PathBuf::from(&config.storage_root)));
    let (update_runtime, update_commands) = UpdateRuntime::new();
    let state = Arc::new(Mutex::new(AppState {
        server_state: ServerState::Stopped,
        shutdown_tx: None,
        server_task: None,
        port: config.port,
        config: config.clone(),
        started_at: Instant::now(),
        update_runtime: update_runtime.clone(),
        telegram: telegram_bridge.clone(),
    }));

    let state_for_exit = state.clone();
    let logs_dir = get_logs_dir();
    let _ = std::fs::create_dir_all(&logs_dir);

    write_agent_log("=== Synthius Dendrite starting ===");
    write_agent_log(&format!("Version: {}", env!("CARGO_PKG_VERSION")));
    write_agent_log(&format!(
        "Bundle type: {:?}",
        tauri::utils::platform::bundle_type()
    ));
    write_agent_log(&format!("Logs directory: {}", logs_dir.display()));

    tauri::Builder::default()
        // Register first: a repeated desktop launch must not start another
        // tray, HTTP service, Telegram bridge or updater. The worker CLI
        // bypasses app_lib::run() and keeps its separate process lifecycle.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || {
                let ready = handle
                    .try_state::<Arc<Mutex<AppState>>>()
                    .is_some_and(|state| {
                        state
                            .lock()
                            .is_ok_and(|state| matches!(state.server_state, ServerState::Running))
                    });
                if ready || handle.get_webview_window("dendrite").is_some() {
                    show_status_window(&handle);
                }
                // During initial startup, the existing startup task opens
                // the window after the loopback service is ready.
            });
        }))
        .invoke_handler(tauri::generate_handler![
            telegram::telegram_native_state,
            telegram::telegram_native_accept_consent,
            telegram::telegram_native_command,
            telegram::telegram_native_discard,
            telegram::telegram_native_register,
            telegram::telegram_native_open_sources
        ])
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_log::Builder::default()
                .level(log::LevelFilter::Info)
                .filter(|metadata| updater::diagnostic_is_visible(metadata.target()))
                .target(tauri_plugin_log::Target::new(
                    tauri_plugin_log::TargetKind::Folder {
                        path: logs_dir.clone(),
                        file_name: Some("tauri.log".to_string()),
                    },
                ))
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .build(),
        )
        .manage(state.clone())
        .manage(telegram_bridge.clone())
        .setup(move |app| {
            let telegram_app = app.handle().clone();
            telegram_bridge.set_window(Box::new(move || telegram::show_window(&telegram_app)));
            let update_app = app.handle().clone();
            let update_state = state.clone();
            let update_runtime_for_worker = update_runtime.clone();
            tauri::async_runtime::spawn(run_update_worker(
                update_app,
                update_state,
                update_runtime_for_worker,
                update_commands,
            ));
            tauri::async_runtime::spawn(run_update_schedule(update_runtime.clone()));

            let config_for_tray = config.clone();
            let config_for_menu = config.clone();
            let storage_size = get_storage_size(&config.storage_root);
            let initial_menu = build_tray_menu(app.handle(), &ServerState::Stopped, storage_size);

            let scale = app
                .primary_monitor()
                .ok()
                .flatten()
                .map(|monitor| monitor.scale_factor())
                .unwrap_or(1.0);
            let icon = desktop_icon(16.0 * scale);

            let _tray = TrayIconBuilder::with_id(TRAY_ID)
                .icon(icon)
                .icon_as_template(cfg!(target_os = "macos"))
                .menu(&initial_menu)
                .tooltip(PRODUCT_NAME)
                .on_menu_event(move |app, event| {
                    let storage_root = config_for_menu.storage_root.clone();
                    match event.id.as_ref() {
                        "quit" => {
                            app.exit(0);
                        }
                        "open_logs" => {
                            let logs_dir = get_logs_dir();
                            let _ = std::fs::create_dir_all(&logs_dir);
                            let _ = open::that(&logs_dir);
                        }
                        "open_status" => show_status_window(app),
                        "open_web" => {
                            if open::that(configured_web_app_url()).is_err() {
                                write_agent_log("Could not open Synthius in the default browser");
                            }
                        }
                        "open_storage" => {
                            let _ = ensure_storage_directories(Path::new(&storage_root));
                            let _ = open::that(&storage_root);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event({
                    let config_for_click = config_for_tray.clone();
                    move |tray, event| {
                        if let TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } = event
                        {
                            let storage_size = get_storage_size(&config_for_click.storage_root);
                            let state_guard = tray.app_handle().state::<Arc<Mutex<AppState>>>();
                            let server_state = state_guard.lock().unwrap().server_state.clone();
                            let new_menu =
                                build_tray_menu(tray.app_handle(), &server_state, storage_size);
                            let _ = tray.set_menu(Some(new_menu));
                            show_status_window(tray.app_handle());
                        }
                    }
                })
                .build(app)?;

            #[cfg(target_os = "windows")]
            windows_icons::refresh(app.handle());

            if config.auto_start {
                let app_handle = app.handle().clone();
                let state_clone = state.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    if let Err(error) = start_backend(&app_handle, state_clone).await {
                        log::error!("Auto-start failed: {error}");
                    } else {
                        show_status_window(&app_handle);
                    }
                });
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |app_handle, event| match event {
            tauri::RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { .. },
                ..
            } if label == "telegram-import" => {
                let bridge = app_handle.state::<Arc<telegram::Bridge>>().inner().clone();
                std::thread::spawn(move || bridge.shutdown());
            }
            tauri::RunEvent::WindowEvent {
                event: WindowEvent::ScaleFactorChanged { .. } | WindowEvent::ThemeChanged(_),
                ..
            } => {
                #[cfg(target_os = "windows")]
                windows_icons::refresh(app_handle);
            }
            tauri::RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "dendrite" => {
                api.prevent_close();
                if let Some(window) = app_handle.get_webview_window(&label) {
                    let _ = window.hide();
                }
            }
            tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. } => {
                app_handle.state::<Arc<telegram::Bridge>>().shutdown();
                log::info!("Application exiting, stopping local runtime...");
                stop_backend(state_for_exit.clone());
            }
            _ => {}
        });
}

#[cfg(test)]
#[path = "unit.rs"]
mod tests;
