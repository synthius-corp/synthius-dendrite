//! Native upload orchestration. Only the process-bound desktop page may submit
//! bytes. Online requests carry a purpose-bound device grant, never an account
//! refresh token, provider key, service key or a UI-supplied URL/path.
use super::*;
use serde_json::Value;
use zeroize::{Zeroize, Zeroizing};

const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Credential {
    user_id: String,
    ticket: String,
    #[serde(default)]
    source_relay_ticket: Option<String>,
    #[cfg(test)]
    #[serde(skip)]
    backend: Option<String>,
}
impl Drop for Credential {
    fn drop(&mut self) {
        self.ticket.zeroize();
        self.source_relay_ticket.zeroize();
    }
}

pub struct UploadRuntime {
    credential: AsyncMutex<Option<Zeroizing<String>>>,
    pub operation: AsyncMutex<()>,
    vault_user: String,
    #[cfg(test)]
    test_backend: Mutex<Option<String>>,
}

impl UploadRuntime {
    pub fn new(device: &str) -> Self {
        Self {
            credential: AsyncMutex::new(None),
            operation: AsyncMutex::new(()),
            vault_user: format!("{:x}:{}", Sha256::digest(configured_supabase_url()), device),
            #[cfg(test)]
            test_backend: Mutex::new(None),
        }
    }
    #[cfg(all(not(test), any(windows, target_os = "macos")))]
    fn vault(&self) -> Result<keyring::Entry, ApiError> {
        keyring::Entry::new("Synthius Dendrite native uploads", &self.vault_user)
            .map_err(|_| ApiError::internal("Secure account storage is unavailable"))
    }
    async fn credential(&self) -> Result<Credential, ApiError> {
        #[allow(unused_mut)] // Secure-store restore mutates this outside tests.
        let mut slot = self.credential.lock().await;
        if slot.is_none() {
            #[cfg(all(not(test), any(windows, target_os = "macos")))]
            if let Ok(raw) = self.vault()?.get_password() {
                *slot = Some(Zeroizing::new(raw));
            }
        }
        let raw = slot.as_ref().ok_or_else(ApiError::auth_required)?;
        #[allow(unused_mut)]
        let mut credential: Credential =
            serde_json::from_str(raw).map_err(|_| ApiError::auth_invalid())?;
        #[cfg(test)]
        {
            credential.backend = self.test_backend.lock().unwrap().clone();
        }
        Ok(credential)
    }
    async fn pair(&self, credential: Credential) -> Result<(), ApiError> {
        let raw = Zeroizing::new(
            serde_json::to_string(&credential)
                .map_err(|_| ApiError::internal("Account connection failed"))?,
        );
        // A failed secure-store write leaves the prior owner/session intact.
        #[cfg(all(not(test), any(windows, target_os = "macos")))]
        self.vault()?
            .set_password(&raw)
            .map_err(|_| ApiError::internal("Secure account storage is unavailable"))?;
        *self.credential.lock().await = Some(raw);
        Ok(())
    }
}

fn backend_base() -> &'static str {
    super::BACKEND_URL
}

fn client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|_| ApiError::internal("Account connection is unavailable"))
}

fn request(
    credential: &Credential,
    method: reqwest::Method,
    path: &str,
) -> Result<reqwest::RequestBuilder, ApiError> {
    let base = backend_base();
    #[cfg(test)]
    let base = credential.backend.as_deref().unwrap_or(base);
    if base.is_empty() {
        return Err(ApiError::bad_request(
            "Build Dendrite for the matching Synthius environment",
        ));
    }
    Ok(client()?
        .request(method, format!("{}/api/dendrite/uploads{}", base, path))
        .header("X-Dendrite-Upload-Ticket", &credential.ticket))
}

async fn response_json(response: reqwest::Response) -> Result<Value, ApiError> {
    let status = response.status();
    if !status.is_success() {
        // Provider/backend errors can contain sensitive data. Keep a bounded,
        // actionable desktop message; neither request nor response is logged.
        return Err(if status.as_u16() == 401 {
            ApiError::auth_expired()
        } else if status.as_u16() == 404 {
            ApiError::not_found("The selected persona or source is unavailable")
        } else {
            ApiError::conflict(
                "Source registration could not finish. Keep this window open and retry.",
            )
        });
    }
    response
        .json()
        .await
        .map_err(|_| ApiError::internal("Account response was incomplete"))
}

async fn get(credential: &Credential, path: &str) -> Result<Value, ApiError> {
    let response = request(credential, reqwest::Method::GET, path)?
        .send()
        .await
        .map_err(|_| ApiError::internal("Connect to the internet and retry this source"))?;
    response_json(response).await
}

async fn connect_source_relay(
    state: &HttpRuntimeState,
    credential: &mut Credential,
) -> Result<(), ApiError> {
    if credential.source_relay_ticket.is_none() {
        let grant = get(credential, "/source-relay-ticket").await?;
        if grant["user_id"] != credential.user_id || grant["device_id"] != state.device_id {
            return Err(ApiError::auth_invalid());
        }
        let ticket = grant["ticket"]
            .as_str()
            .filter(|value| !value.is_empty() && value.len() <= 4096)
            .ok_or_else(ApiError::auth_invalid)?;
        credential.source_relay_ticket = Some(ticket.to_string());
        state.uploads.pair(credential.clone()).await?;
    }
    state.source_relay.connect(
        state.storage_root.clone(),
        state.device_id.clone(),
        credential.user_id.clone(),
        credential.source_relay_ticket.clone().unwrap(),
    );
    Ok(())
}

pub async fn restore_source_relay(state: &HttpRuntimeState) {
    // Upgrade older paired installations once; subsequent restarts can start
    // the socket offline and let its reconnect loop wait for the network.
    for attempt in 0..3 {
        let result = {
            let _operation = state.uploads.operation.lock().await;
            match state.uploads.credential().await {
                Ok(mut credential) => connect_source_relay(state, &mut credential).await,
                Err(_) => return, // Not paired: no network work until pairing.
            }
        };
        match result {
            Ok(()) => return,
            Err(error)
                if error.status == StatusCode::UNAUTHORIZED
                    || error.status == StatusCode::FORBIDDEN =>
            {
                return
            }
            Err(_) if attempt < 2 => tokio::time::sleep(Duration::from_secs(1 << attempt)).await,
            Err(_) => write_agent_log(
                "WARNING: Local search connection could not be restored; reopen Sources to retry.",
            ),
        }
    }
}

async fn post(credential: &Credential, path: &str, body: &Value) -> Result<Value, ApiError> {
    let response = request(credential, reqwest::Method::POST, path)?
        .json(body)
        .send()
        .await
        .map_err(|_| ApiError::internal("Connect to the internet and retry this source"))?;
    response_json(response).await
}

async fn catalog_confirms_source_absent(credential: &Credential, file_id: &str) -> bool {
    get(credential, "/catalog")
        .await
        .ok()
        .and_then(|catalog| catalog["sources"].as_array().cloned())
        .is_some_and(|sources| sources.iter().all(|source| source["id"] != file_id))
}

async fn sync_catalog(state: &HttpRuntimeState, credential: &Credential) {
    if let Ok(value) = get(credential, "/catalog").await {
        if let Ok(catalog) = serde_json::from_value::<SourceCatalogPayload>(value) {
            if validate_source_catalog(&catalog).is_ok() {
                let stored = StoredSourceCatalog {
                    user_id: credential.user_id.clone(),
                    synced_at: chrono::Utc::now().to_rfc3339(),
                    sources: catalog.sources,
                };
                if let Ok(bytes) = serde_json::to_vec(&stored) {
                    let _ = write_storage_file_atomically(
                        source_catalog_path(&state.storage_root),
                        Bytes::from(bytes),
                        true,
                    )
                    .await;
                }
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectRequest {
    ticket: String,
}

pub async fn connect_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(body): Json<ConnectRequest>,
) -> Result<Json<Value>, ApiError> {
    let user = authorize_request(&headers, &state).await?;
    if body.ticket.is_empty() || body.ticket.len() > 4096 {
        return Err(ApiError::bad_request("Invalid upload capability"));
    }
    let _operation = state.uploads.operation.lock().await;
    let mut credential = Credential {
        user_id: user.clone(),
        ticket: body.ticket,
        source_relay_ticket: None,
        #[cfg(test)]
        backend: None,
    };
    let context = get(&credential, "/context").await?;
    if context["user_id"] != user || context["device_id"] != state.device_id {
        return Err(ApiError::forbidden("Account/device connection mismatch"));
    }
    state.uploads.pair(credential.clone()).await?;
    state
        .source_relay
        .disconnect_other_owner(&credential.user_id);
    // Upload pairing remains usable against older backends without this route.
    if connect_source_relay(&state, &mut credential).await.is_err() {
        write_agent_log("WARNING: Local search connection is not ready after account pairing.");
    }
    Ok(Json(json!({"status":"connected"})))
}

#[derive(Clone, Serialize, Deserialize)]
struct Source {
    user_id: String,
    persona_id: String,
    file_id: String,
    batch_id: String,
    filename: String,
    stored_filename: String,
    storage_path: String,
    size: usize,
    sha256: String,
    category: String,
    subtype: String,
    confidence: f64,
    hints: Vec<String>,
    source_usage: String,
    registered: bool,
    complete: bool,
    #[serde(default)]
    subject_name: String,
}

fn source_path(root: &Path, owner: &str, id: &str) -> Result<PathBuf, ApiError> {
    Uuid::parse_str(owner).map_err(|_| ApiError::auth_invalid())?;
    Uuid::parse_str(id).map_err(|_| ApiError::bad_request("Invalid source identifier"))?;
    // Reuse the vault path/symlink fence for metadata as well as source bytes.
    let (_, path) = resolve_storage_file(
        &root.join("metadata"),
        StorageBucket::Uploads,
        &format!("{owner}/.native-uploads/{id}.json"),
    )?;
    Ok(path)
}

async fn save(state: &HttpRuntimeState, source: &Source) -> Result<(), ApiError> {
    let bytes = serde_json::to_vec(source)
        .map_err(|_| ApiError::internal("Source progress could not be saved"))?;
    write_storage_file_atomically(
        source_path(&state.storage_root, &source.user_id, &source.file_id)?,
        Bytes::from(bytes),
        true,
    )
    .await
}

async fn load(state: &HttpRuntimeState, owner: &str, id: &str) -> Result<Source, ApiError> {
    let bytes = read_bounded_local_source(source_path(&state.storage_root, owner, id)?).await?;
    if bytes.len() > 32 * 1024 {
        return Err(ApiError::bad_request("Source progress is invalid"));
    }
    let source: Source = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::bad_request("Source progress is invalid"))?;
    validate_source(&source, owner)?;
    Ok(source)
}

fn validate_source(source: &Source, owner: &str) -> Result<(), ApiError> {
    for id in [&source.file_id, &source.persona_id, &source.batch_id] {
        Uuid::parse_str(id).map_err(|_| ApiError::bad_request("Invalid source identity"))?;
    }
    let expected = format!("{owner}/{}/{}", source.file_id, source.stored_filename);
    if source.user_id != owner
        || source.storage_path != expected
        || source.stored_filename.contains(['/', '\\'])
        || normalize_storage_path(&expected)? != expected
        || source.size > MAX_FILE_BYTES
        || source.sha256.len() != 64
        || !source.sha256.chars().all(|c| c.is_ascii_hexdigit())
        || !matches!(
            source.source_usage.as_str(),
            "persona_source_and_context" | "context_only"
        )
    {
        return Err(ApiError::forbidden(
            "Source progress does not match this account",
        ));
    }
    Ok(())
}

async fn pending(
    state: &HttpRuntimeState,
    owner: &str,
    context: &Value,
    catalog: Option<&Value>,
) -> Vec<Value> {
    let Ok(path) = source_path(&state.storage_root, owner, &Uuid::nil().to_string()) else {
        return vec![];
    };
    let Some(parent) = path.parent() else {
        return vec![];
    };
    let Ok(mut entries) = tokio::fs::read_dir(parent).await else {
        return vec![];
    };
    let mut rows = vec![];
    while let Ok(Some(entry)) = entries.next_entry().await {
        if rows.len() >= 500 {
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(id) = name.strip_suffix(".json") {
            if let Ok(source) = load(state, owner, id).await {
                if source.complete {
                    continue;
                }
                let persona_exists = context["personas"]
                    .as_array()
                    .is_some_and(|p| p.iter().any(|p| p["id"] == source.persona_id));
                let still_local = catalog
                    .and_then(|c| c["sources"].as_array())
                    .map(|sources| {
                        sources.iter().any(|s| {
                            s["id"] == source.file_id
                                && s["persona_id"] == source.persona_id
                                && s["storage_mode"] == "local"
                                && s["storage_device_id"] == state.device_id
                        })
                    });
                // Hide progress for sources removed/migrated elsewhere without
                // claiming parsing completed. A source moved back can resume.
                if !persona_exists || (source.registered && still_local == Some(false)) {
                    continue;
                }
                rows.push(json!(source));
            }
        }
    }
    rows
}

pub async fn context_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    authorize_update_action(&headers, &state)?;
    let credential = state.uploads.credential().await?;
    let mut context = get(&credential, "/context").await?;
    if context["user_id"] != credential.user_id || context["device_id"] != state.device_id {
        return Err(ApiError::auth_invalid());
    }
    // Native Sources also repairs a missed first upgrade handshake. Never
    // expose the search capability to the page's response.
    let _operation = state.uploads.operation.lock().await;
    let mut current = state.uploads.credential().await?;
    if current.user_id != credential.user_id {
        return Err(ApiError::auth_invalid());
    }
    let _ = connect_source_relay(&state, &mut current).await;
    drop(_operation);
    let catalog = get(&credential, "/catalog").await.ok();
    context["pending"] =
        json!(pending(&state, &credential.user_id, &context, catalog.as_ref()).await);
    Ok(Json(context))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramOpenRequest {
    user_id: Uuid,
    persona_id: Uuid,
    source_usage: String,
    resume_job_id: Option<Uuid>,
}

pub async fn telegram_open_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(body): Json<TelegramOpenRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize_update_action(&headers, &state)?;
    if !matches!(
        body.source_usage.as_str(),
        "persona_source_and_context" | "context_only"
    ) {
        return Err(ApiError::bad_request("Select a valid source use"));
    }
    let credential = state.uploads.credential().await?;
    if body.user_id.to_string() != credential.user_id {
        return Err(ApiError::auth_invalid());
    }
    let value = post(
        &credential,
        "/telegram-context",
        &json!({"persona_id":body.persona_id}),
    )
    .await?;
    let context: synthius_dendrite_telegram::protocol::Context =
        serde_json::from_value(value).map_err(|_| ApiError::auth_invalid())?;
    validate_telegram_context(
        &context,
        &credential.user_id,
        &state.device_id,
        body.persona_id,
    )?;
    let consent = super::telegram::ConsentAccess::new(
        context.user_id,
        request(&credential, reqwest::Method::GET, "/telegram-consent")?
            .timeout(Duration::from_secs(20)),
        request(&credential, reqwest::Method::POST, "/telegram-consent")?
            .timeout(Duration::from_secs(20))
            .json(&json!({"accepted":true})),
    );
    let accepted = consent
        .accepted(false)
        .await
        .map_err(ApiError::bad_request)?;
    let bridge = state.telegram.clone();
    let id = tokio::task::spawn_blocking(move || {
        bridge.open(
            context,
            body.source_usage,
            body.resume_job_id,
            consent,
            accepted,
        )
    })
    .await
    .map_err(|_| ApiError::internal("Telegram snapshot is unavailable"))?
    .map_err(ApiError::bad_request)?;
    Ok(Json(json!({"job_id":id})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramRegisterRequest {
    job_id: Uuid,
}

fn telegram_source_matches(
    source: &Source,
    journal: &synthius_dendrite_telegram::journal::Journal,
    manifest: &synthius_dendrite_telegram::journal::Source,
) -> bool {
    source.user_id == journal.context.user_id.to_string()
        && source.persona_id == journal.context.persona_id.to_string()
        && source.file_id == manifest.file_id.to_string()
        && source.batch_id == journal.job_id.to_string()
        && source.filename == manifest.original_filename
        && source.stored_filename == manifest.stored_filename
        && source.storage_path == manifest.storage_path
        && source.size == manifest.size
        && source.sha256 == manifest.sha256
        && source.source_usage == journal.source_usage
        && source.category == "chat"
        && source.subtype == "telegram_json"
}

async fn register_telegram_job(
    state: &HttpRuntimeState,
    credential: &Credential,
    job_id: Uuid,
) -> Result<Value, ApiError> {
    let user_id = Uuid::parse_str(&credential.user_id).map_err(|_| ApiError::auth_invalid())?;
    let device_id =
        Uuid::parse_str(&state.device_id).map_err(|_| ApiError::internal("Device unavailable"))?;
    let journal = state
        .telegram
        .load_owned(user_id, device_id, job_id)
        .map_err(ApiError::bad_request)?;
    if !journal.complete || journal.cancelled {
        return Err(ApiError::conflict("Wait for the snapshot to finish"));
    }
    let context = get(credential, "/context").await?;
    require_upload_context(&context, &credential.user_id, &state.device_id)?;
    if !context["personas"].as_array().is_some_and(|personas| {
        personas
            .iter()
            .any(|persona| persona["id"] == journal.context.persona_id.to_string())
    }) {
        return Err(ApiError::not_found("The selected persona is unavailable"));
    }

    let mut added = 0usize;
    let mut duplicates = 0usize;
    for manifest in journal.sources.clone() {
        if state
            .telegram
            .load_owned(user_id, device_id, job_id)
            .map_err(ApiError::bad_request)?
            .registered
            .contains(&manifest.file_id)
        {
            continue;
        }
        let id = manifest.file_id.to_string();
        let metadata_path = source_path(&state.storage_root, &credential.user_id, &id)?;
        let mut source = if tokio::fs::try_exists(&metadata_path)
            .await
            .map_err(ApiError::from_io)?
        {
            let source = load(state, &credential.user_id, &id).await?;
            if !telegram_source_matches(&source, &journal, &manifest) {
                return Err(ApiError::conflict(
                    "Telegram source progress does not match its snapshot",
                ));
            }
            source
        } else {
            let (_, path) = resolve_storage_file(
                &state.storage_root,
                StorageBucket::Uploads,
                &manifest.storage_path,
            )?;
            let bytes = read_bounded_local_source(path).await?;
            if bytes.len() != manifest.size
                || format!("{:x}", Sha256::digest(&bytes)) != manifest.sha256
            {
                return Err(ApiError::conflict(
                    "Telegram source does not match its saved snapshot",
                ));
            }
            let Json(classification) = classify_local_content(
                manifest.original_filename.clone(),
                bytes,
                state.device_id.clone(),
            )
            .await?;
            if classification.category != "chat" || classification.subtype != "telegram_json" {
                return Err(ApiError::conflict(
                    "Telegram snapshot format could not be verified",
                ));
            }
            let source = Source {
                user_id: credential.user_id.clone(),
                persona_id: journal.context.persona_id.to_string(),
                file_id: id.clone(),
                batch_id: journal.job_id.to_string(),
                filename: manifest.original_filename.clone(),
                stored_filename: manifest.stored_filename.clone(),
                storage_path: manifest.storage_path.clone(),
                size: manifest.size,
                sha256: manifest.sha256.clone(),
                category: classification.category,
                subtype: classification.subtype,
                confidence: classification.confidence,
                hints: classification.hints,
                source_usage: journal.source_usage.clone(),
                registered: false,
                complete: false,
                subject_name: String::new(),
            };
            save(state, &source).await?;
            source
        };

        if !source.registered && register(state, credential, &mut source).await?.is_some() {
            // The backend has confirmed an identical existing source. Record
            // that durable outcome before removing the redundant local bytes.
            state
                .telegram
                .mark_registered(user_id, device_id, job_id, &manifest)
                .map_err(ApiError::bad_request)?;
            discard_duplicate_upload(state, &source).await?;
            duplicates += 1;
        } else {
            prepare_source(
                state,
                credential,
                ParseRequest {
                    user_id: credential.user_id.clone(),
                    file_id: source.file_id.clone(),
                    category: "chat".into(),
                    subject_name: String::new(),
                },
            )
            .await?;
            state
                .telegram
                .mark_registered(user_id, device_id, job_id, &manifest)
                .map_err(ApiError::bad_request)?;
            added += 1;
        }
    }
    sync_catalog(state, credential).await;
    let finalized = state
        .telegram
        .load_owned(user_id, device_id, job_id)
        .map_err(ApiError::bad_request)?;
    Ok(json!({
        "status":"ready",
        "persona_id":finalized.context.persona_id,
        "persona_name":finalized.context.persona_name,
        "sources":finalized.sources.len(),
        "registered":finalized.registered.len(),
        "added":added,
        "duplicates":duplicates,
        "persona_url":format!("{}/personas?selected={}", configured_web_app_url().trim_end_matches("/files"), finalized.context.persona_id),
    }))
}

/// Publish completed Telegram snapshots as ordinary Local sources. The
/// operation is idempotent across lost acknowledgements: the immutable journal,
/// native-upload receipt and backend row must all agree before progress moves.
pub async fn telegram_register_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(body): Json<TelegramRegisterRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize_update_action(&headers, &state)?;
    let _operation = state.uploads.operation.lock().await;
    let credential = state.uploads.credential().await?;
    Ok(Json(
        register_telegram_job(&state, &credential, body.job_id).await?,
    ))
}

fn validate_telegram_context(
    context: &synthius_dendrite_telegram::protocol::Context,
    owner: &str,
    device: &str,
    persona: Uuid,
) -> Result<(), ApiError> {
    if context.user_id.to_string() != owner
        || context.device_id.to_string() != device
        || context.persona_id != persona
    {
        return Err(ApiError::auth_invalid());
    }
    Ok(())
}

async fn register(
    state: &HttpRuntimeState,
    credential: &Credential,
    source: &mut Source,
) -> Result<Option<String>, ApiError> {
    let (_, full) = resolve_storage_file(
        &state.storage_root,
        StorageBucket::Uploads,
        &source.storage_path,
    )?;
    let bytes = read_bounded_local_source(full).await?;
    if bytes.len() != source.size || format!("{:x}", Sha256::digest(bytes)) != source.sha256 {
        return Err(ApiError::conflict(
            "Local source changed since classification",
        ));
    }
    let registered = post(credential, "/register", &json!({"file_id":source.file_id,"persona_id":source.persona_id,
        "upload_batch_id":source.batch_id,"stored_filename":source.stored_filename,"original_filename":source.filename,
        "file_size":source.size,"content_sha256":source.sha256,"device_id":state.device_id,
        "category":source.category,"subtype":source.subtype,"confidence":source.confidence,"hints":source.hints})).await?;
    if registered["duplicate"] == true {
        let existing_id = registered["id"]
            .as_str()
            .filter(|id| Uuid::parse_str(id).is_ok() && *id != source.file_id)
            .ok_or_else(|| ApiError::conflict("Duplicate source identity is invalid"))?
            .to_string();
        // Adding the same content as a persona source is a monotonic request:
        // an existing context-only source is promoted instead of duplicated.
        // Adding it as context-only never downgrades an existing persona source.
        if source.source_usage == "persona_source_and_context"
            && registered["source_usage"] == "context_only"
        {
            post(
                credential,
                &format!("/{existing_id}/usage"),
                &json!({"persona_id":source.persona_id,"source_usage":source.source_usage}),
            )
            .await?;
        }
        return Ok(Some(existing_id));
    }
    if source.source_usage == "context_only" {
        post(
            credential,
            &format!("/{}/usage", source.file_id),
            &json!({"persona_id":source.persona_id,"source_usage":source.source_usage}),
        )
        .await?;
    }
    source.registered = true;
    save(state, source).await?;
    Ok(None)
}

async fn discard_duplicate_upload(
    state: &HttpRuntimeState,
    source: &Source,
) -> Result<(), ApiError> {
    let (_, bytes_path) = resolve_storage_file(
        &state.storage_root,
        StorageBucket::Uploads,
        &source.storage_path,
    )?;
    if tokio::fs::try_exists(&bytes_path)
        .await
        .map_err(ApiError::from_io)?
    {
        let bytes = read_bounded_local_source(bytes_path.clone()).await?;
        if bytes.len() != source.size || format!("{:x}", Sha256::digest(bytes)) != source.sha256 {
            return Err(ApiError::conflict(
                "Duplicate staging content changed before cleanup",
            ));
        }
        tokio::fs::remove_file(bytes_path)
            .await
            .map_err(ApiError::from_io)?;
    }
    let journal = source_path(&state.storage_root, &source.user_id, &source.file_id)?;
    if tokio::fs::try_exists(&journal)
        .await
        .map_err(ApiError::from_io)?
    {
        tokio::fs::remove_file(journal)
            .await
            .map_err(ApiError::from_io)?;
    }
    Ok(())
}

pub async fn add_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiError> {
    authorize_update_action(&headers, &state)?;
    let _operation = state.uploads.operation.lock().await;
    let credential = state.uploads.credential().await?;
    let mut fields = HashMap::new();
    let mut content = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| ApiError::bad_request("File could not be read"))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            if content.is_some() {
                return Err(ApiError::bad_request("Upload one file at a time"));
            }
            let bytes = field
                .bytes()
                .await
                .map_err(|_| ApiError::bad_request("File could not be read"))?;
            if bytes.len() > MAX_FILE_BYTES {
                return Err(ApiError::bad_request("Maximum size is 50 MB per file"));
            }
            content = Some(bytes);
        } else {
            if fields.contains_key(&name) {
                return Err(ApiError::bad_request("Duplicate upload field"));
            }
            let text = field
                .text()
                .await
                .map_err(|_| ApiError::bad_request("Invalid upload field"))?;
            if text.len() > 1024 {
                return Err(ApiError::bad_request("Upload field is too long"));
            }
            fields.insert(name, text);
        }
    }
    let required = |key: &str| {
        fields
            .get(key)
            .filter(|v| !v.is_empty())
            .cloned()
            .ok_or_else(|| ApiError::bad_request("Missing upload identity"))
    };
    if required("user_id")? != credential.user_id {
        return Err(ApiError::forbidden(
            "The connected account changed. Reopen Add sources.",
        ));
    }
    let persona = required("persona_id")?;
    let id = required("file_id")?;
    let batch = required("batch_id")?;
    for value in [&persona, &id, &batch] {
        Uuid::parse_str(value).map_err(|_| ApiError::bad_request("Invalid source identity"))?;
    }
    let context = get(&credential, "/context").await?;
    require_upload_context(&context, &credential.user_id, &state.device_id)?;
    if !context["personas"]
        .as_array()
        .is_some_and(|p| p.iter().any(|p| p["id"] == persona))
    {
        return Err(ApiError::not_found("Choose an owned persona"));
    }
    let filename = required("filename")?;
    if filename.contains(['/', '\\', '\0']) {
        return Err(ApiError::bad_request("Invalid source filename"));
    }
    let extension = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .filter(|e| e.len() <= 12 && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .ok_or_else(|| ApiError::bad_request("Choose a supported source format"))?;
    if !context["extensions"]
        .as_array()
        .is_some_and(|e| e.iter().any(|e| e.as_str() == Some(&extension)))
    {
        return Err(ApiError::bad_request("Choose a supported source format"));
    }
    let content = content.ok_or_else(|| ApiError::bad_request("Choose a source file"))?;
    let sha256 = format!("{:x}", Sha256::digest(&content));
    let usage = required("source_usage")?;
    if !matches!(
        usage.as_str(),
        "persona_source_and_context" | "context_only"
    ) {
        return Err(ApiError::bad_request("Invalid source use"));
    }
    // Retry the same identity after lost acknowledgements without a second row.
    if tokio::fs::try_exists(source_path(&state.storage_root, &credential.user_id, &id)?)
        .await
        .map_err(ApiError::from_io)?
    {
        let mut prior = load(&state, &credential.user_id, &id).await?;
        if prior.persona_id != persona
            || prior.batch_id != batch
            || prior.sha256 != sha256
            || prior.filename != filename
            || prior.source_usage != usage
        {
            return Err(ApiError::conflict(
                "This source identity belongs to a different upload",
            ));
        }
        if !prior.registered {
            let (_, full) = resolve_storage_file(
                &state.storage_root,
                StorageBucket::Uploads,
                &prior.storage_path,
            )?;
            if !tokio::fs::try_exists(&full)
                .await
                .map_err(ApiError::from_io)?
            {
                write_storage_file_atomically(full, content, false).await?;
            }
            if let Some(existing_id) = register(&state, &credential, &mut prior).await? {
                discard_duplicate_upload(&state, &prior).await?;
                sync_catalog(&state, &credential).await;
                return Ok(Json(json!({
                    "file_id": existing_id,
                    "filename": prior.filename,
                    "complete": true,
                    "duplicate": true,
                })));
            }
        }
        sync_catalog(&state, &credential).await;
        return Ok(Json(json!(prior)));
    }
    let Json(classification) =
        classify_local_content(filename.clone(), content.to_vec(), state.device_id.clone()).await?;
    let stored = format!("{id}.{extension}");
    let path = format!("{}/{id}/{stored}", credential.user_id);
    let mut source = Source {
        user_id: credential.user_id.clone(),
        persona_id: persona,
        file_id: id,
        batch_id: batch,
        filename,
        stored_filename: stored,
        storage_path: path.clone(),
        size: content.len(),
        sha256: sha256.clone(),
        category: classification.category,
        subtype: classification.subtype,
        confidence: classification.confidence,
        hints: classification.hints,
        source_usage: usage,
        registered: false,
        complete: false,
        subject_name: String::new(),
    };
    save(&state, &source).await?;
    let (_, full) = resolve_storage_file(&state.storage_root, StorageBucket::Uploads, &path)?;
    if tokio::fs::try_exists(&full)
        .await
        .map_err(ApiError::from_io)?
    {
        let existing = read_bounded_local_source(full.clone()).await?;
        if format!("{:x}", Sha256::digest(existing)) != sha256 {
            return Err(ApiError::conflict(
                "A different local source already exists",
            ));
        }
    } else {
        write_storage_file_atomically(full, content.clone(), false).await?;
    }
    if let Some(existing_id) = register(&state, &credential, &mut source).await? {
        discard_duplicate_upload(&state, &source).await?;
        sync_catalog(&state, &credential).await;
        return Ok(Json(json!({
            "file_id": existing_id,
            "filename": source.filename,
            "complete": true,
            "duplicate": true,
        })));
    }
    sync_catalog(&state, &credential).await;
    Ok(Json(json!(source)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParseRequest {
    user_id: String,
    file_id: String,
    category: String,
    subject_name: String,
}

fn require_upload_context(context: &Value, owner: &str, device: &str) -> Result<(), ApiError> {
    if context["user_id"] != owner || context["device_id"] != device {
        return Err(ApiError::auth_invalid());
    }
    if context["upload_allowed"] != true {
        return Err(ApiError::forbidden(
            "Complete your Synthius account setup before adding sources",
        ));
    }
    Ok(())
}

async fn materialize_index(
    state: &HttpRuntimeState,
    credential: &Credential,
    source: &Source,
    manifest: Value,
) -> Result<(), ApiError> {
    if manifest["status"] == "ready" {
        return Ok(());
    }
    if manifest["status"] != "materializing"
        || manifest["device_id"] != state.device_id
        || manifest["source_file_id"] != source.file_id
    {
        return Err(ApiError::conflict("Source index transfer is incomplete"));
    }
    let path = manifest["storage_path"]
        .as_str()
        .ok_or_else(|| ApiError::bad_request("Invalid index path"))?;
    if !storage_path_belongs_to_user(path, &credential.user_id) {
        return Err(ApiError::forbidden("Index owner mismatch"));
    }
    let (_, full) = resolve_storage_file(&state.storage_root, StorageBucket::Outputs, path)?;
    let mut response = request(
        credential,
        reqwest::Method::GET,
        &format!(
            "/{}/index/bytes?persona_id={}",
            source.file_id, source.persona_id
        ),
    )?
    .send()
    .await
    .map_err(|_| ApiError::internal("Source index transfer is unavailable"))?;
    if !response.status().is_success() {
        return Err(ApiError::conflict("Source index transfer is unavailable"));
    }
    let expected = manifest["size"]
        .as_u64()
        .filter(|size| *size <= 512 * 1024 * 1024)
        .ok_or_else(|| ApiError::conflict("Source index size is invalid"))?;
    if response
        .content_length()
        .is_some_and(|size| size != expected)
    {
        return Err(ApiError::conflict("Source index size mismatch"));
    }
    let mut collected = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ApiError::internal("Source index transfer was interrupted"))?
    {
        if collected.len() as u64 + chunk.len() as u64 > expected {
            return Err(ApiError::conflict("Source index size mismatch"));
        }
        collected.extend_from_slice(&chunk);
    }
    let bytes = Bytes::from(collected);
    if manifest["size"].as_u64() != Some(bytes.len() as u64)
        || manifest["sha256"].as_str() != Some(&format!("{:x}", Sha256::digest(&bytes)))
    {
        return Err(ApiError::conflict("Source index integrity check failed"));
    }
    if tokio::fs::try_exists(&full)
        .await
        .map_err(ApiError::from_io)?
    {
        if tokio::fs::read(&full).await.map_err(ApiError::from_io)? != bytes {
            return Err(ApiError::conflict(
                "A different source index occupies this path",
            ));
        }
    } else {
        write_storage_file_atomically(full, bytes, false).await?;
    }
    post(
        credential,
        &format!("/{}/index/confirm", source.file_id),
        &json!({"persona_id":source.persona_id,
        "device_id":state.device_id,"size":manifest["size"],"sha256":manifest["sha256"]}),
    )
    .await?;
    Ok(())
}

pub async fn parse_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(body): Json<ParseRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize_update_action(&headers, &state)?;
    let _operation = state.uploads.operation.lock().await;
    let credential = state.uploads.credential().await?;
    Ok(Json(prepare_source(&state, &credential, body).await?))
}

async fn prepare_source(
    state: &HttpRuntimeState,
    credential: &Credential,
    body: ParseRequest,
) -> Result<Value, ApiError> {
    if body.user_id != credential.user_id {
        return Err(ApiError::forbidden(
            "The connected account changed. Reopen Add sources.",
        ));
    }
    let mut source = load(state, &credential.user_id, &body.file_id).await?;
    if body.subject_name.len() > 500 {
        return Err(ApiError::bad_request("Subject name is too long"));
    }
    let context = get(credential, "/context").await?;
    require_upload_context(&context, &credential.user_id, &state.device_id)?;
    if !context["personas"]
        .as_array()
        .is_some_and(|p| p.iter().any(|p| p["id"] == source.persona_id))
    {
        return Err(ApiError::not_found("Choose an owned persona"));
    }
    let category = context["categories"]
        .as_array()
        .and_then(|categories| categories.iter().find(|c| c["value"] == body.category))
        .ok_or_else(|| ApiError::bad_request("Choose a source category"))?;
    if category["needs_subject_name"] == true && body.subject_name.trim().is_empty() {
        return Err(ApiError::bad_request(
            "Enter the name of the person this source describes",
        ));
    }
    if !source.registered {
        if let Some(existing_id) = register(state, credential, &mut source).await? {
            discard_duplicate_upload(state, &source).await?;
            sync_catalog(state, credential).await;
            return Ok(json!({"status":"duplicate","source_file_id":existing_id,"duplicate":true}));
        }
    }
    let current = get(
        credential,
        &format!(
            "/{}/details?persona_id={}",
            source.file_id, source.persona_id
        ),
    )
    .await?;
    // A committed parse's acknowledgement may have been lost. Resume its index
    // handoff instead of reparsing/reuploading/rebuilding embeddings.
    if current["status"] == "ready" || current["status"] == "processed" {
        if current["source_search_status"] == "local_transfer_pending" {
            let manifest = get(
                credential,
                &format!("/{}/index?persona_id={}", source.file_id, source.persona_id),
            )
            .await?;
            materialize_index(state, credential, &source, manifest).await?;
        }
        source.complete = true;
        save(state, &source).await?;
        return Ok(json!({"status":"ready"}));
    }
    let output = format!(
        "{}/corpus/{}/corpus.jsonl",
        credential.user_id, source.file_id
    );
    let Json(parsed) = parse_source_for_owner(
        state.clone(),
        &credential.user_id,
        ParserParseRequest {
            source_file_id: source.file_id.clone(),
            source_path: source.storage_path.clone(),
            output_path: output.clone(),
            original_filename: source.filename.clone(),
            category: body.category.clone(),
            subtype: source.subtype.clone(),
            subject_name: Some(body.subject_name.clone()),
        },
    )
    .await?;
    source.category = body.category.clone();
    source.subject_name = body.subject_name.clone();
    save(state, &source).await?;
    if parsed.source_sha256 != source.sha256 {
        return Err(ApiError::conflict(
            "Local source changed since classification",
        ));
    }
    let (_, full) = resolve_storage_file(&state.storage_root, StorageBucket::Uploads, &output)?;
    let bytes = read_bounded_local_source(full).await?;
    if bytes.len() != parsed.corpus_size
        || format!("{:x}", Sha256::digest(&bytes)) != parsed.corpus_sha256
    {
        return Err(ApiError::conflict("Parsed source integrity mismatch"));
    }
    let mut form = reqwest::multipart::Form::new().part(
        "corpus",
        reqwest::multipart::Part::bytes(bytes).file_name("corpus.jsonl"),
    );
    for (key, value) in [
        ("persona_id", source.persona_id.clone()),
        ("device_id", state.device_id.clone()),
        ("source_sha256", source.sha256.clone()),
        ("category", body.category.clone()),
        ("subtype", parsed.subtype),
        ("corpus_path", output),
        ("corpus_sha256", parsed.corpus_sha256),
        ("corpus_size", parsed.corpus_size.to_string()),
        ("parser_id", parsed.parser_id),
        ("parser_version", parsed.parser_version),
        ("corpus_schema", parsed.corpus_schema),
        ("message_count", parsed.message_count.to_string()),
        ("word_count", parsed.word_count.to_string()),
        (
            "participants_json",
            serde_json::to_string(&parsed.participants).unwrap(),
        ),
        ("subject_name", body.subject_name.clone()),
    ] {
        form = form.text(key, value);
    }
    if let Some(date) = parsed.date_start {
        form = form.text("date_start", date);
    }
    if let Some(date) = parsed.date_end {
        form = form.text("date_end", date);
    }
    let response = request(
        credential,
        reqwest::Method::POST,
        &format!("/{}/corpus", source.file_id),
    )?
    .multipart(form)
    .send()
    .await
    .map_err(|_| {
        ApiError::internal("Registration was interrupted. Retry to resume this source.")
    })?;
    let result = response_json(response).await?;
    source.category = body.category;
    source.subject_name = body.subject_name;
    save(state, &source).await?;
    if result["source_search_materialization"]["status"] == "materializing" {
        materialize_index(
            state,
            credential,
            &source,
            result["source_search_materialization"].clone(),
        )
        .await?;
    }
    source.complete = true;
    save(state, &source).await?;
    // Refresh the native source view; a catalogue outage cannot undo success.
    sync_catalog(state, credential).await;
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceActionRequest {
    user_id: String,
    persona_id: String,
    file_id: String,
    action: String,
    source_usage: Option<String>,
}

fn deletion_targets(
    state: &HttpRuntimeState,
    owner: &str,
    plan: &Value,
) -> Result<Vec<PathBuf>, ApiError> {
    let artifacts = plan["local_artifacts"]
        .as_array()
        .ok_or_else(|| ApiError::conflict("Source cleanup plan was incomplete"))?;
    if artifacts.len() > 32 || (!artifacts.is_empty() && plan["local_device_id"] != state.device_id)
    {
        return Err(ApiError::forbidden(
            "Open this source's registered Dendrite device",
        ));
    }
    artifacts
        .iter()
        .map(|artifact| {
            let bucket = StorageBucket::parse(artifact["bucket"].as_str().unwrap_or(""))?;
            let (normalized, full) = resolve_storage_file(
                &state.storage_root,
                bucket,
                artifact["storage_path"].as_str().unwrap_or(""),
            )?;
            if !storage_path_belongs_to_user(&normalized, owner) {
                return Err(ApiError::forbidden("Source cleanup account mismatch"));
            }
            Ok(full)
        })
        .collect()
}

/// Desktop actions receive identifiers only. The backend supplies and fences
/// the deletion plan; the web lifecycle remains authoritative for all locks.
pub async fn source_action_handler(
    State(state): State<HttpRuntimeState>,
    headers: HeaderMap,
    Json(body): Json<SourceActionRequest>,
) -> Result<Json<Value>, ApiError> {
    authorize_update_action(&headers, &state)?;
    let _operation = state.uploads.operation.lock().await;
    let credential = state.uploads.credential().await?;
    if body.user_id != credential.user_id {
        return Err(ApiError::forbidden(
            "The connected account changed. Refresh Sources.",
        ));
    }
    for id in [&body.file_id, &body.persona_id] {
        Uuid::parse_str(id).map_err(|_| ApiError::bad_request("Invalid source identity"))?;
    }
    let result = match body.action.as_str() {
        "usage" => {
            let usage = body.source_usage.as_deref().unwrap_or("");
            if !matches!(usage, "persona_source_and_context" | "context_only") {
                return Err(ApiError::bad_request("Invalid source use"));
            }
            post(
                &credential,
                &format!("/{}/usage", body.file_id),
                &json!({"persona_id":body.persona_id,"source_usage":usage}),
            )
            .await?
        }
        "delete" => {
            let plan = post(
                &credential,
                &format!(
                    "/{}/deletion/prepare?persona_id={}",
                    body.file_id, body.persona_id
                ),
                &json!({}),
            )
            .await
            .map_err(|error| {
                if matches!(
                    error.status,
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
                ) {
                    error
                } else {
                    ApiError::conflict(
                        "Source deletion could not start. Finish active processing or moving, then retry.",
                    )
                }
            })?;
            if plan["file_id"] != body.file_id || plan["status"] != "claimed" {
                return Err(ApiError::conflict("Source cleanup plan was incomplete"));
            }
            let token = plan["deletion_token"].as_str().unwrap_or("");
            Uuid::parse_str(token)
                .map_err(|_| ApiError::conflict("Source cleanup claim was incomplete"))?;
            // Validate every path before removing any, and tolerate already
            // removed bytes when resuming an interrupted deletion.
            for target in deletion_targets(&state, &credential.user_id, &plan)? {
                match tokio::fs::remove_file(target).await {
                    Ok(()) => (),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(ApiError::from_io(error)),
                }
            }
            let finalize_path = format!(
                "/{}/deletion/finalize?persona_id={}&deletion_token={}",
                body.file_id, body.persona_id, token
            );
            let mut finalized = None;
            let mut last_error = None;
            for attempt in 0..3 {
                match post(&credential, &finalize_path, &json!({})).await {
                    Ok(value) => {
                        finalized = Some(value);
                        break;
                    }
                    Err(error) => {
                        let retryable = matches!(
                            error.status,
                            StatusCode::CONFLICT
                                | StatusCode::INTERNAL_SERVER_ERROR
                                | StatusCode::SERVICE_UNAVAILABLE
                        );
                        last_error = Some(error);
                        if !retryable || attempt == 2 {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(250 * (attempt + 1) as u64)).await;
                    }
                }
            }
            if let Some(value) = finalized {
                value
            } else if catalog_confirms_source_absent(&credential, &body.file_id).await {
                // Finalization may have committed even if its HTTP response was
                // lost. The authoritative catalogue makes that outcome safe to
                // treat as success instead of asking the user to delete again.
                json!({"file_id":body.file_id,"status":"deleted"})
            } else {
                let error = last_error.unwrap_or_else(|| {
                    ApiError::conflict("Source deletion could not be confirmed")
                });
                write_agent_log(&format!(
                    "Source deletion did not finish after bounded retries status={}",
                    error.status.as_u16()
                ));
                return Err(
                    if matches!(
                        error.status,
                        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
                    ) {
                        error
                    } else {
                        ApiError::conflict(
                        "Source cleanup is still incomplete. Keep Dendrite connected and retry.",
                    )
                    },
                );
            }
        }
        _ => return Err(ApiError::bad_request("Invalid source action")),
    };
    sync_catalog(&state, &credential).await;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_relay_restores_after_restart_without_browser_or_backend_grant_refresh() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let root = tempfile::tempdir().unwrap();
            let owner = Uuid::new_v4().to_string();
            let device = Uuid::new_v4().to_string();
            let grant = Arc::new(Mutex::new(json!({"user_id":"wrong-owner","device_id":device,"ticket":"search-grant"})));
            async fn ticket(State(grant):State<Arc<Mutex<Value>>>, headers:HeaderMap) -> Json<Value> {
                assert_eq!(headers["x-dendrite-upload-ticket"], "upload-grant");
                Json(grant.lock().unwrap().clone())
            }
            let listener = TcpListener::bind(("127.0.0.1",0)).await.unwrap();
            let backend_url = format!("http://{}", listener.local_addr().unwrap());
            let router = Router::new().route("/api/dendrite/uploads/source-relay-ticket", axum::routing::get(ticket)).with_state(grant.clone());
            let backend = tokio::spawn(async move { axum::serve(listener,router).await.unwrap(); });
            let listener = TcpListener::bind(("127.0.0.1",0)).await.unwrap();
            let socket_url = format!("ws://{}", listener.local_addr().unwrap());
            let (sent, mut received) = tokio::sync::mpsc::channel(2);
            let expected_owner = owner.clone(); let expected_device = device.clone();
            let socket_server = tokio::spawn(async move {
                for _ in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut socket = accept_async(stream).await.unwrap();
                    let hello = socket.next().await.unwrap().unwrap();
                    assert_eq!(serde_json::from_str::<Value>(hello.to_text().unwrap()).unwrap()["ticket"], "search-grant");
                    socket.send(Message::Text(json!({"type":"ready","user_id":expected_owner,"device_id":expected_device}).to_string().into())).await.unwrap();
                    socket.send(Message::Text(json!({"id":"probe","nonce":"nonce","user_id":expected_owner,"device_id":expected_device,
                        "expires_at":chrono::Utc::now().timestamp() + 8,"operation":"invalid-synthetic-operation"}).to_string().into())).await.unwrap();
                    loop {
                        let body = socket.next().await.unwrap().unwrap();
                        let value:Value = serde_json::from_str(body.to_text().unwrap()).unwrap();
                        if value["type"] == "heartbeat" { continue; }
                        assert_eq!(value["id"], "probe"); assert!(value["error"].is_string());
                        sent.send(()).await.unwrap(); break;
                    }
                }
            });
            let make_state = |raw:String| {
                let uploads = Arc::new(UploadRuntime::new(&device));
                *uploads.test_backend.lock().unwrap() = Some(backend_url.clone());
                *uploads.credential.try_lock().unwrap() = Some(Zeroizing::new(raw));
                let relay = Arc::new(source_search::RelayRuntime::default());
                *relay.test_socket_url.lock().unwrap() = Some(socket_url.clone());
                let (update_runtime,_) = UpdateRuntime::new();
                HttpRuntimeState {storage_root:root.path().to_path_buf(),device_id:device.clone(),port:0,started_at:Instant::now(),
                    http_client:reqwest::Client::new(),jwks_cache:Arc::new(AsyncMutex::new(JwksCache::default())),update_runtime,
                    telegram:Arc::new(telegram::Bridge::new(root.path().to_path_buf())),source_relay:relay,uploads}
            };
            let state = make_state(json!({"user_id":owner,"ticket":"upload-grant"}).to_string());
            let mut credential = state.uploads.credential().await.unwrap();
            assert!(connect_source_relay(&state,&mut credential).await.is_err());
            assert!(state.uploads.credential().await.unwrap().source_relay_ticket.is_none());
            grant.lock().unwrap()["user_id"] = json!(owner);
            restore_source_relay(&state).await;
            tokio::time::timeout(Duration::from_secs(3),received.recv()).await.unwrap().unwrap();
            let saved = state.uploads.credential.lock().await.as_ref().unwrap().to_string();
            state.source_relay.disconnect();
            backend.abort(); // A restart must use the saved grant, not a browser/HTTP refresh.
            let restarted = make_state(saved);
            restore_source_relay(&restarted).await;
            tokio::time::timeout(Duration::from_secs(3),received.recv()).await.unwrap().unwrap();
            restarted.source_relay.disconnect();
            socket_server.await.unwrap();
        });
    }
    #[test]
    fn native_source_identity_is_owner_fenced() {
        let owner = Uuid::new_v4().to_string();
        let id = Uuid::new_v4().to_string();
        let stored = format!("{id}.txt");
        let mut source = Source {
            user_id: owner.clone(),
            persona_id: Uuid::new_v4().to_string(),
            file_id: id.clone(),
            batch_id: Uuid::new_v4().to_string(),
            filename: "Notes.txt".into(),
            stored_filename: stored.clone(),
            storage_path: format!("{owner}/{id}/{stored}"),
            size: 20,
            sha256: "a".repeat(64),
            category: "notes".into(),
            subtype: "plain_text".into(),
            confidence: 0.9,
            hints: vec![],
            source_usage: "context_only".into(),
            registered: false,
            complete: false,
            subject_name: String::new(),
        };
        assert!(validate_source(&source, &owner).is_ok());
        assert!(validate_source(&source, &Uuid::new_v4().to_string()).is_err());
        source.storage_path = format!("{owner}/../escape");
        assert!(validate_source(&source, &owner).is_err());
    }
    #[test]
    fn upload_credentials_are_project_and_device_separated() {
        let a = UploadRuntime::new(&Uuid::new_v4().to_string());
        let b = UploadRuntime::new(&Uuid::new_v4().to_string());
        assert_ne!(a.vault_user, b.vault_user);
        assert!(
            backend_base().starts_with("https://") || backend_base() == "http://127.0.0.1:8000"
        );
    }

    #[test]
    fn native_upload_setup_is_checked_before_local_processing() {
        let owner = Uuid::new_v4().to_string();
        let device = Uuid::new_v4().to_string();
        let mut context = json!({"user_id":owner,"device_id":device,"upload_allowed":true});
        assert!(require_upload_context(&context, &owner, &device).is_ok());
        context["upload_allowed"] = json!(false);
        assert!(require_upload_context(&context, &owner, &device).is_err());
        context["upload_allowed"] = Value::Null;
        assert!(require_upload_context(&context, &owner, &device).is_err());
        context["upload_allowed"] = json!(true);
        assert!(require_upload_context(&context, &Uuid::new_v4().to_string(), &device).is_err());
    }

    #[test]
    fn native_telegram_intent_binds_owner_persona_and_device() {
        let context = synthius_dendrite_telegram::protocol::Context {
            user_id: Uuid::new_v4(),
            persona_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            persona_name: "Synthetic".into(),
        };
        let owner = context.user_id.to_string();
        let device = context.device_id.to_string();
        assert!(validate_telegram_context(&context, &owner, &device, context.persona_id).is_ok());
        assert!(validate_telegram_context(
            &context,
            &Uuid::new_v4().to_string(),
            &device,
            context.persona_id
        )
        .is_err());
        assert!(validate_telegram_context(
            &context,
            &owner,
            &Uuid::new_v4().to_string(),
            context.persona_id
        )
        .is_err());
        assert!(validate_telegram_context(&context, &owner, &device, Uuid::new_v4()).is_err());
    }

    #[test]
    fn completed_telegram_snapshot_registers_once_as_a_local_source() {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let owner = Uuid::new_v4();
            let persona = Uuid::new_v4();
            let device = Uuid::new_v4();
            let file_id = Uuid::new_v4();
            let bytes = serde_json::to_vec(&json!({
                "name": "Synthetic Telegram chat",
                "type": "telegram_snapshot",
                "id": 42,
                "messages": [{
                    "id": 1,
                    "type": "message",
                    "date": "2026-01-01T10:00:00Z",
                    "from": "Synthetic Alex",
                    "from_id": "synthetic-user",
                    "text": "Synthetic Telegram memory about a language lesson."
                }]
            }))
            .unwrap();
            let stored_filename = format!("{file_id}.json");
            let storage_path = format!("{owner}/{file_id}/{stored_filename}");
            let full_path = root.path().join("uploads").join(&storage_path);
            std::fs::create_dir_all(full_path.parent().unwrap()).unwrap();
            std::fs::write(&full_path, &bytes).unwrap();

            let mut journal = synthius_dendrite_telegram::journal::Journal::new(
                synthius_dendrite_telegram::protocol::Context {
                    user_id: owner,
                    persona_id: persona,
                    device_id: device,
                    persona_name: "Synthetic Persona".into(),
                },
                "persona_source_and_context".into(),
            )
            .unwrap();
            journal.complete = true;
            journal
                .sources
                .push(synthius_dendrite_telegram::journal::Source {
                    file_id,
                    stored_filename: stored_filename.clone(),
                    original_filename: "Telegram snapshot synthetic.json".into(),
                    storage_path: storage_path.clone(),
                    size: bytes.len(),
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                });
            journal.save(root.path()).unwrap();

            let backend_state = Arc::new(Mutex::new(json!({
                "owner": owner,
                "persona": persona,
                "device": device,
                "file_id": file_id,
                "registers": 0
            })));
            async fn backend(
                State(state): State<Arc<Mutex<Value>>>,
                request: axum::extract::Request,
            ) -> Response {
                assert_eq!(
                    request.headers().get("x-dendrite-upload-ticket").unwrap(),
                    "synthetic-native-ticket"
                );
                let path = request
                    .uri()
                    .path()
                    .trim_start_matches("/api/dendrite/uploads/");
                match path {
                    "context" => {
                        let state = state.lock().unwrap();
                        Json(json!({
                            "user_id": state["owner"],
                            "device_id": state["device"],
                            "personas": [{"id": state["persona"], "name": "Synthetic Persona"}],
                            "upload_allowed": true,
                            "categories": [{"value": "chat", "needs_subject_name": false}]
                        }))
                        .into_response()
                    }
                    "register" => {
                        let body = axum::body::to_bytes(request.into_body(), 32 * 1024)
                            .await
                            .unwrap();
                        let value: Value = serde_json::from_slice(&body).unwrap();
                        let mut state = state.lock().unwrap();
                        assert_eq!(value["file_id"], state["file_id"]);
                        assert_eq!(value["persona_id"], state["persona"]);
                        assert_eq!(value["device_id"], state["device"]);
                        state["registers"] = json!(state["registers"].as_u64().unwrap() + 1);
                        Json(json!({"id": state["file_id"], "duplicate": false})).into_response()
                    }
                    "catalog" => Json(json!({})).into_response(),
                    _ if path.ends_with("/details") => Json(json!({
                        "status": "classified",
                        "source_search_status": "pending"
                    }))
                    .into_response(),
                    _ if path.ends_with("/corpus") => Json(json!({
                        "status": "ready",
                        "source_search_materialization": {"status": "ready"}
                    }))
                    .into_response(),
                    _ => StatusCode::NOT_FOUND.into_response(),
                }
            }

            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let backend_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
            let backend_router = Router::new()
                .route("/api/dendrite/uploads/*path", axum::routing::any(backend))
                .with_state(backend_state.clone());
            let backend_task =
                tokio::spawn(async move { axum::serve(listener, backend_router).await.unwrap() });

            let uploads = Arc::new(UploadRuntime::new(&device.to_string()));
            let (update_runtime, _) = UpdateRuntime::new();
            let state = HttpRuntimeState {
                storage_root: root.path().to_path_buf(),
                device_id: device.to_string(),
                port: 0,
                started_at: Instant::now(),
                http_client: reqwest::Client::new(),
                jwks_cache: Arc::new(AsyncMutex::new(JwksCache::default())),
                update_runtime,
                telegram: Arc::new(telegram::Bridge::new(root.path().to_path_buf())),
                source_relay: Arc::new(source_search::RelayRuntime::default()),
                uploads,
            };
            let credential = Credential {
                user_id: owner.to_string(),
                ticket: "synthetic-native-ticket".into(),
                source_relay_ticket: None,
                backend: Some(backend_url),
            };

            let first = register_telegram_job(&state, &credential, journal.job_id)
                .await
                .unwrap();
            let second = register_telegram_job(&state, &credential, journal.job_id)
                .await
                .unwrap();
            assert_eq!(first["sources"], 1);
            assert_eq!(first["registered"], 1);
            assert_eq!(second["registered"], 1);
            assert_eq!(backend_state.lock().unwrap()["registers"], 1);
            assert_eq!(
                state
                    .telegram
                    .load_owned(owner, device, journal.job_id)
                    .unwrap()
                    .registered,
                vec![file_id]
            );
            assert!(
                load(&state, &owner.to_string(), &file_id.to_string())
                    .await
                    .unwrap()
                    .registered
            );
            assert!(
                load(&state, &owner.to_string(), &file_id.to_string())
                    .await
                    .unwrap()
                    .complete
            );
            backend_task.abort();
        });
    }

    #[test]
    fn native_upload_roundtrip_retries_recovery_and_index_handoff() {
        let root = tempfile::tempdir().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let owner=Uuid::new_v4().to_string();let persona=Uuid::new_v4().to_string();let device=Uuid::new_v4().to_string();
            let backend_state=Arc::new(Mutex::new(json!({"owner":owner,"persona":persona,"device":device,"files":{},"registers":0,"corpora":0,"confirmations":0,"lose_ack":true})));
            async fn backend(State(state):State<Arc<Mutex<Value>>>,req:axum::extract::Request)->Response {
                assert_eq!(req.headers().get("x-dendrite-upload-ticket").unwrap(),"synthetic-native-ticket");
                let path=req.uri().path().trim_start_matches("/api/dendrite/uploads/").to_string();
                if path=="context" {let s=state.lock().unwrap();return Json(json!({"user_id":s["owner"],"device_id":s["device"],
                    "personas":[{"id":s["persona"],"name":"Synthetic Persona"}],"upload_allowed":true,"extensions":["txt","md"],"categories":[{"value":"notes","label":"Notes","needs_subject_name":true}],"max_file_bytes":MAX_FILE_BYTES})).into_response();}
                if path=="catalog" {let s=state.lock().unwrap();let sources:Vec<Value>=s["files"].as_object().unwrap().iter().map(|(id,row)|json!({
                    "id":id,"persona_id":s["persona"],"persona_name":"Synthetic Persona","filename":"notes.txt","file_size":125,
                    "status":row["status"],"category":"notes","source_kind":"upload","source_usage":"context_only",
                    "storage_mode":row.get("storage_mode").cloned().unwrap_or(json!("local")),"storage_health":"present","storage_device_id":s["device"],
                    "storage_path":format!("{}/{id}/{id}.txt",s["owner"].as_str().unwrap()),"storage_transition_target":null,"storage_transition_status":null,"created_at":null})).collect();
                    return Json(json!({"generated_at":"2026-10-06T10:00:00Z","sources":sources})).into_response();}
                if path=="register" {
                    let body=axum::body::to_bytes(req.into_body(),32*1024).await.unwrap();let row:Value=serde_json::from_slice(&body).unwrap();
                    let mut s=state.lock().unwrap();assert_eq!(row["device_id"],s["device"]);assert_eq!(row["persona_id"],s["persona"]);
                    let id=row["file_id"].as_str().unwrap().to_string();s["registers"]=json!(s["registers"].as_u64().unwrap()+1);
                    if let Some((existing_id,_))=s["files"].as_object().unwrap().iter().find(|(existing_id,existing)|
                        existing_id.as_str()!=id && existing["content_sha256"]==row["content_sha256"])
                    {return Json(json!({"id":existing_id,"duplicate":true})).into_response();}
                    s["files"][&id]=json!({"status":"classified","content_sha256":row["content_sha256"]});
                    return Json(json!({"id":id,"duplicate":false})).into_response();
                }
                let id=path.split('/').next().unwrap().to_string();
                if path.ends_with("/deletion/prepare") {
                    let s=state.lock().unwrap();let owner=s["owner"].as_str().unwrap();
                    return Json(json!({"status":"claimed","file_id":id,"deletion_token":Uuid::nil().to_string(),"local_device_id":s["device"],
                        "local_artifacts":[{"bucket":"uploads","storage_path":format!("{owner}/{id}/{id}.txt")},
                        {"bucket":"uploads","storage_path":format!("{owner}/corpus/{id}/corpus.jsonl")},
                        {"bucket":"outputs","storage_path":format!("{owner}/source-indexes/{id}/source-index.sqlite.gz")}]})).into_response();
                }
                if path.ends_with("/deletion/finalize") {
                    let mut s=state.lock().unwrap();
                    if s["retry_delete"]!=true{s["retry_delete"]=json!(true);return StatusCode::SERVICE_UNAVAILABLE.into_response();}
                    s["files"].as_object_mut().unwrap().remove(&id);
                    return Json(json!({"file_id":id,"status":"deleted"})).into_response();
                }
                if path.ends_with("/details") {return Json(state.lock().unwrap()["files"][&id].clone()).into_response();}
                if path.ends_with("/usage") {return Json(json!({"status":"saved"})).into_response();}
                if path.ends_with("/corpus") {
                    let mut form=<Multipart as axum::extract::FromRequest<()>>::from_request(req,&()).await.unwrap();let mut body=HashMap::new();
                    while let Some(field)=form.next_field().await.unwrap(){let name=field.name().unwrap().to_string();body.insert(name,field.bytes().await.unwrap());}
                    let bytes=body.get("corpus").unwrap();let digest=format!("{:x}",Sha256::digest(bytes));
                    assert_eq!(std::str::from_utf8(body.get("corpus_sha256").unwrap()).unwrap(),digest);
                    assert_eq!(std::str::from_utf8(body.get("corpus_size").unwrap()).unwrap(),bytes.len().to_string());
                    assert!(std::str::from_utf8(bytes).unwrap().lines().all(|line|serde_json::from_str::<Value>(line).is_ok()));
                    let mut s=state.lock().unwrap();s["corpora"]=json!(s["corpora"].as_u64().unwrap()+1);s["files"][&id]["status"]=json!("ready");s["files"][&id]["source_search_status"]=json!("local_transfer_pending");
                    if s["lose_ack"]==true{s["lose_ack"]=json!(false);return StatusCode::SERVICE_UNAVAILABLE.into_response();}
                }
                if path.ends_with("/index/bytes") {return Bytes::from_static(b"synthetic local index").into_response();}
                if path.ends_with("/index/confirm") {let mut s=state.lock().unwrap();s["confirmations"]=json!(s["confirmations"].as_u64().unwrap()+1);s["files"][&id]["source_search_status"]=json!("ready");return Json(json!({"status":"ready"})).into_response();}
                let s=state.lock().unwrap();let artifact=b"synthetic local index";
                let manifest=json!({"status":"materializing","source_file_id":id,"device_id":s["device"],
                    "storage_path":format!("{}/source-indexes/{id}/source-index.sqlite.gz",s["owner"].as_str().unwrap()),"size":artifact.len(),"sha256":format!("{:x}",Sha256::digest(artifact))});
                if path.ends_with("/index") {return Json(manifest).into_response();}
                if path.ends_with("/corpus") {return Json(json!({"source_search_materialization":manifest})).into_response();}
                StatusCode::NOT_FOUND.into_response()
            }
            let backend_listener=TcpListener::bind(("127.0.0.1",0)).await.unwrap();let backend_url=format!("http://127.0.0.1:{}",backend_listener.local_addr().unwrap().port());
            let backend_router=Router::new().route("/api/dendrite/uploads/*path",axum::routing::any(backend)).with_state(backend_state.clone());
            let backend_task=tokio::spawn(async move{axum::serve(backend_listener,backend_router).await.unwrap();});
            let listener=TcpListener::bind(("127.0.0.1",0)).await.unwrap();let port=listener.local_addr().unwrap().port();let base=format!("http://127.0.0.1:{port}");
            let uploads=Arc::new(UploadRuntime::new(&device));*uploads.test_backend.lock().unwrap()=Some(backend_url);
            *uploads.credential.lock().await=Some(Zeroizing::new(json!({"user_id":owner,"ticket":"synthetic-native-ticket"}).to_string()));
            let (update_runtime,_)=UpdateRuntime::new();let token=update_runtime.page_token().to_string();
            let state=HttpRuntimeState{storage_root:root.path().to_path_buf(),device_id:device.clone(),port,started_at:Instant::now(),http_client:reqwest::Client::new(),
                jwks_cache:Arc::new(AsyncMutex::new(JwksCache::default())),update_runtime,telegram:Arc::new(telegram::Bridge::new(root.path().to_path_buf())),source_relay:Arc::new(source_search::RelayRuntime::default()),uploads:uploads.clone()};
            let cleanup_state = state.clone();
            let native_task=tokio::spawn(async move{axum::serve(listener,build_http_router(state)).await.unwrap();});
            let http=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
            let send=|path:&str|http.post(format!("{base}/api/desktop/uploads/{path}")).header("origin",&base).header("X-Dendrite-Update-Token",&token);
            let context=send("context").send().await.unwrap().json::<Value>().await.unwrap();assert_eq!(context["user_id"],owner);
            let unauthorized=http.post(format!("{base}/api/desktop/uploads/context")).send().await.unwrap();assert_eq!(unauthorized.status(),StatusCode::FORBIDDEN);
            let wrong_origin=http.post(format!("{base}/api/desktop/uploads/context")).header("origin","https://attacker.example").header("X-Dendrite-Update-Token",&token).send().await.unwrap();assert_eq!(wrong_origin.status(),StatusCode::FORBIDDEN);
            let id=Uuid::new_v4().to_string();let batch=Uuid::new_v4().to_string();
            let form=|who:&str,source_id:&str|{let mut form=reqwest::multipart::Form::new().part("file",reqwest::multipart::Part::bytes(b"I enjoy gardening at home. Every spring I plant herbs and tomatoes with my sister and spend weekends tending the garden.".to_vec()).file_name("notes.txt"));
                for(k,v)in [("user_id",who),("persona_id",&persona),("file_id",source_id),("batch_id",&batch),("filename","notes.txt"),("source_usage","context_only")]{form=form.text(k,v.to_string());}form};
            let response=send("add").multipart(form(&owner,&id)).send().await.unwrap();assert_eq!(response.status(),StatusCode::OK,"{}",response.text().await.unwrap_or_default());
            assert_eq!(send("add").multipart(form(&owner,&id)).send().await.unwrap().status(),StatusCode::OK);
            assert_eq!(backend_state.lock().unwrap()["registers"],1);
            assert_eq!(send("context").send().await.unwrap().json::<Value>().await.unwrap()["pending"].as_array().unwrap().len(),1);
            backend_state.lock().unwrap()["files"][&id]["storage_mode"]=json!("cloud");
            assert_eq!(send("context").send().await.unwrap().json::<Value>().await.unwrap()["pending"].as_array().unwrap().len(),0);
            backend_state.lock().unwrap()["files"][&id]["storage_mode"]=json!("local");
            assert_eq!(send("context").send().await.unwrap().json::<Value>().await.unwrap()["pending"].as_array().unwrap().len(),1,"a returned Local source retains unfinished parse progress");
            assert_eq!(send("add").multipart(form(&Uuid::new_v4().to_string(),&id)).send().await.unwrap().status(),StatusCode::FORBIDDEN);
            let body=json!({"user_id":owner,"file_id":id,"category":"notes","subject_name":"Synthetic Persona"});
            // The backend commits but its acknowledgement is lost. Retrying
            // must finish the index without a second parse/index build.
            assert_eq!(send("parse").json(&body).send().await.unwrap().status(),StatusCode::CONFLICT);
            assert_eq!(send("parse").json(&body).send().await.unwrap().status(),StatusCode::OK);
            assert_eq!(send("parse").json(&body).send().await.unwrap().status(),StatusCode::OK);
            assert_eq!(backend_state.lock().unwrap()["corpora"],1);assert_eq!(backend_state.lock().unwrap()["confirmations"],1);
            let duplicate_id=Uuid::new_v4().to_string();
            let duplicate=send("add").multipart(form(&owner,&duplicate_id)).send().await.unwrap();assert_eq!(duplicate.status(),StatusCode::OK);
            let duplicate=duplicate.json::<Value>().await.unwrap();assert_eq!(duplicate["duplicate"],true);assert_eq!(duplicate["file_id"],id);
            assert!(!root.path().join(format!("uploads/{owner}/{duplicate_id}/{duplicate_id}.txt")).exists());
            assert!(!source_path(root.path(),&owner,&duplicate_id).unwrap().exists());
            assert_eq!(backend_state.lock().unwrap()["files"].as_object().unwrap().len(),1);
            let context=send("context").send().await.unwrap().json::<Value>().await.unwrap();assert_eq!(context["pending"].as_array().unwrap().len(),0);
            assert!(root.path().join(format!("uploads/{owner}/{id}/{id}.txt")).is_file());
            assert!(root.path().join(format!("uploads/{owner}/corpus/{id}/corpus.jsonl")).is_file());
            assert!(root.path().join(format!("outputs/{owner}/source-indexes/{id}/source-index.sqlite.gz")).is_file());
            // Validate the entire plan before deleting even its first valid path.
            let original_path = format!("{owner}/{id}/{id}.txt");
            let valid = json!({"bucket":"uploads","storage_path":original_path});
            for invalid in [
                json!({"bucket":"uploads","storage_path":format!("{}/{id}/{id}.txt",Uuid::new_v4())}),
                json!({"bucket":"uploads","storage_path":format!("{owner}/../escape")}),
                json!({"bucket":"unknown","storage_path":original_path}),
            ] {
                let plan = json!({"local_device_id":device,"local_artifacts":[valid,invalid]});
                assert!(deletion_targets(&cleanup_state, &owner, &plan).is_err());
                assert!(root.path().join(format!("uploads/{original_path}")).is_file());
            }
            let foreign_device = json!({"local_device_id":Uuid::new_v4(),"local_artifacts":[valid]});
            assert!(deletion_targets(&cleanup_state, &owner, &foreign_device).is_err());
            let action=|body:&Value|send("source-action").json(body);
            let mut body=json!({"user_id":owner,"persona_id":persona,"file_id":id,"action":"usage","source_usage":"persona_source_and_context"});
            assert_eq!(action(&body).send().await.unwrap().status(),StatusCode::OK);
            body["user_id"]=json!(Uuid::new_v4().to_string());
            assert_eq!(action(&body).send().await.unwrap().status(),StatusCode::FORBIDDEN);
            body["user_id"]=json!(owner);body["action"]=json!("delete");body["source_usage"]=Value::Null;
            // Files are removed and a transient finalization failure is retried
            // against the same claim without asking the user to delete again.
            assert_eq!(action(&body).send().await.unwrap().status(),StatusCode::OK);
            assert!(!root.path().join(format!("uploads/{owner}/{id}/{id}.txt")).exists());
            assert!(!root.path().join(format!("uploads/{owner}/corpus/{id}/corpus.jsonl")).exists());
            assert!(!root.path().join(format!("outputs/{owner}/source-indexes/{id}/source-index.sqlite.gz")).exists());
            assert!(backend_state.lock().unwrap()["files"].as_object().unwrap().is_empty());
            native_task.abort();backend_task.abort();
        });
    }
}
