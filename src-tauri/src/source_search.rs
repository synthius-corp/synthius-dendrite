//! Local retrieval and resumable vector checkpoints. Provider keys stay on the backend.
use flate2::read::GzDecoder;
use futures_util::{SinkExt, StreamExt};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

const MAX_INDEX_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;
type SearchResult<T> = Result<T, &'static str>;

#[derive(Default)]
pub struct RelayRuntime {
    active: Mutex<Option<ActiveRelay>>,
    #[cfg(test)]
    pub test_socket_url: Mutex<Option<String>>,
}

struct ActiveRelay {
    owner: String,
    ticket: Arc<Mutex<zeroize::Zeroizing<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl RelayRuntime {
    pub fn disconnect_other_owner(&self, user_id: &str) {
        let mut active = self.active.lock().unwrap();
        if active
            .as_ref()
            .is_some_and(|current| current.owner != user_id)
        {
            if let Some(current) = active.take() {
                current.task.abort();
            }
        }
    }

    #[cfg(test)]
    pub fn disconnect(&self) {
        if let Some(current) = self.active.lock().unwrap().take() {
            current.task.abort();
        }
    }

    pub fn connect(
        self: &Arc<Self>,
        root: std::path::PathBuf,
        device: String,
        user_id: String,
        ticket: String,
    ) {
        let mut active = self.active.lock().unwrap();
        if let Some(current) = active.as_ref() {
            if current.owner == user_id && !current.task.is_finished() {
                *current.ticket.lock().unwrap() = zeroize::Zeroizing::new(ticket);
                return;
            }
        }
        if let Some(current) = active.take() {
            current.task.abort();
        }
        let owner = user_id.clone();
        let ticket = Arc::new(Mutex::new(zeroize::Zeroizing::new(ticket)));
        let current = ticket.clone();
        #[allow(unused_mut)]
        let mut socket_url = backend_socket_url(&device);
        #[cfg(test)]
        if let Some(url) = self.test_socket_url.lock().unwrap().as_ref() {
            socket_url = url.clone();
        }
        let task = tokio::spawn(async move {
            let mut delay = 2;
            loop {
                let credential = current.lock().unwrap().clone();
                let started = std::time::Instant::now();
                let _ = serve(&root, &device, &user_id, &credential, &socket_url).await;
                if started.elapsed() > Duration::from_secs(60) {
                    delay = 2;
                }
                tokio::time::sleep(Duration::from_secs(delay)).await;
                delay = (delay * 2).min(60);
            }
        });
        *active = Some(ActiveRelay {
            owner,
            ticket,
            task,
        });
    }
}

impl Drop for RelayRuntime {
    fn drop(&mut self) {
        if let Some(current) = self.active.get_mut().unwrap().take() {
            current.task.abort();
        }
    }
}

fn backend_socket_url(device_id: &str) -> String {
    let base = super::BACKEND_URL
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{base}/api/storage/source-relay/{device_id}")
}

async fn serve(
    root: &Path,
    device: &str,
    user_id: &str,
    ticket: &str,
    socket_url: &str,
) -> SearchResult<()> {
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), connect_async(socket_url))
        .await
        .map_err(|_| "local_source_index_unavailable")?
        .map_err(|_| "local_source_index_unavailable")?;
    socket
        .send(Message::Text(json!({"ticket":ticket}).to_string().into()))
        .await
        .map_err(|_| "local_source_index_unavailable")?;
    let first = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .map_err(|_| "local_source_index_unavailable")?
        .ok_or("local_source_index_unavailable")?
        .map_err(|_| "local_source_index_unavailable")?;
    let ready: Value = serde_json::from_str(
        first
            .to_text()
            .map_err(|_| "local_source_index_unavailable")?,
    )
    .map_err(|_| "local_source_index_unavailable")?;
    if ready["type"] != "ready" || ready["user_id"] != user_id || ready["device_id"] != device {
        return Err("local_source_index_unavailable");
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                socket.send(Message::Text(json!({"type":"heartbeat"}).to_string().into())).await
                    .map_err(|_| "local_source_index_unavailable")?;
            }
            message = socket.next() => {
                let message = message.ok_or("local_source_index_unavailable")?
                    .map_err(|_| "local_source_index_unavailable")?;
                if message.is_close() { return Err("local_source_index_unavailable"); }
                if !message.is_text() { continue; }
                let text = message.to_text().map_err(|_| "source_index_read_failed")?;
                if text.len() > 64 * 1024 { return Err("source_index_read_failed"); }
                let body: Value = serde_json::from_str(text).map_err(|_| "source_index_read_failed")?;
                if body["user_id"] != user_id || body["device_id"] != device { continue; }
                let expires = body["expires_at"].as_f64().ok_or("source_index_read_failed")?;
                if expires < chrono::Utc::now().timestamp_millis() as f64 / 1000.0 { continue; }
                let root = root.to_path_buf();
                let owner = user_id.to_string();
                let device = device.to_string();
                let id = body["id"].clone(); let nonce = body["nonce"].clone();
                let result = tokio::task::spawn_blocking(move || execute(&root, &owner, &device, &body))
                    .await.map_err(|_| "source_index_read_failed")?;
                let response = match result {
                    Ok(result) => json!({"id":id,"nonce":nonce,"result":result}),
                    Err(error) => json!({"id":id,"nonce":nonce,"error":error}),
                }.to_string();
                if response.len() > MAX_REPLY_BYTES { return Err("source_index_read_failed"); }
                socket.send(Message::Text(response.into())).await.map_err(|_| "local_source_index_unavailable")?;
            }
        }
    }
}

fn string<'a>(body: &'a Value, field: &str) -> SearchResult<&'a str> {
    body[field].as_str().ok_or("source_index_read_failed")
}

struct OpenedIndex {
    // Fields drop in declaration order: release SQLite's Windows handle before
    // unlinking its temporary plaintext reader file.
    connection: Connection,
    _file: tempfile::NamedTempFile,
}

fn open_index(
    root: &Path,
    user_id: &str,
    device_id: &str,
    body: &Value,
) -> SearchResult<OpenedIndex> {
    let descriptor = &body["descriptor"];
    if descriptor["user_id"] != user_id
        || descriptor["storage_device_id"] != device_id
        || descriptor["storage_mode"] != "local"
        || descriptor["status"] != "ready"
    {
        return Err("source_index_read_failed");
    }
    let path = string(descriptor, "storage_path")?;
    if !super::storage_path_belongs_to_user(path, user_id) {
        return Err("source_index_read_failed");
    }
    let (_, resolved) = super::resolve_storage_file(root, super::StorageBucket::Outputs, path)
        .map_err(|_| "source_index_read_failed")?;
    let mut file = std::fs::File::open(&resolved).map_err(|_| "source_index_read_failed")?;
    let size = file
        .metadata()
        .map_err(|_| "source_index_read_failed")?
        .len();
    if size == 0
        || size > MAX_INDEX_BYTES
        || descriptor["artifact_size_bytes"].as_u64() != Some(size)
    {
        return Err("source_index_read_failed");
    }
    let mut compressed = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_INDEX_BYTES + 1)
        .read_to_end(&mut compressed)
        .map_err(|_| "source_index_read_failed")?;
    if compressed.len() as u64 != size
        || format!("{:x}", Sha256::digest(&compressed)) != string(descriptor, "artifact_sha256")?
    {
        return Err("source_index_read_failed");
    }
    // Temporary plaintext exists only inside the already-local, owner-scoped vault.
    let mut temporary =
        tempfile::NamedTempFile::new_in(resolved.parent().ok_or("source_index_read_failed")?)
            .map_err(|_| "source_index_read_failed")?;
    let copied = std::io::copy(
        &mut GzDecoder::new(&compressed[..]).take(MAX_INDEX_BYTES + 1),
        temporary.as_file_mut(),
    )
    .map_err(|_| "source_index_read_failed")?;
    if copied > MAX_INDEX_BYTES {
        return Err("source_index_read_failed");
    }
    let connection = Connection::open_with_flags(
        temporary.path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "source_index_read_failed")?;
    connection
        .execute_batch("pragma query_only=on; pragma trusted_schema=off;")
        .map_err(|_| "source_index_read_failed")?;
    let mut statement = connection
        .prepare("select key,value from index_metadata")
        .map_err(|_| "source_index_read_failed")?;
    let metadata = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|_| "source_index_read_failed")?
        .collect::<Result<HashMap<_, _>, _>>()
        .map_err(|_| "source_index_read_failed")?;
    for (field, key) in [
        ("index_format_version", "format_version"),
        ("user_id", "user_id"),
        ("source_file_id", "source_file_id"),
        ("source_fingerprint", "source_fingerprint"),
        ("embedding_provider", "embedding_provider"),
        ("embedding_model", "embedding_model"),
        ("embedding_dimensions", "embedding_dimensions"),
        ("document_count", "document_count"),
        ("chunk_count", "chunk_count"),
    ] {
        let expected = descriptor[field]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| descriptor[field].to_string());
        if metadata.get(key) != Some(&expected) {
            return Err("source_index_read_failed");
        }
    }
    drop(statement);
    Ok(OpenedIndex {
        connection,
        _file: temporary,
    })
}

const SELECT: &str = "select c.chunk_id,c.document_id,d.source_file_id,d.corpus_file_id,d.filename,d.category,
 c.content,c.normalized_content,c.line_start,c.line_end,c.date_start,c.date_end,c.entry_ids_json,c.embedding
 from chunks c join documents d on d.document_id=c.document_id";

struct Chunk {
    value: Value,
    content: String,
    normalized: String,
    vector: Option<Vec<f32>>,
}

fn chunks(connection: &Connection, body: &Value) -> SearchResult<Vec<Chunk>> {
    let mut statement = connection
        .prepare(SELECT)
        .map_err(|_| "source_index_read_failed")?;
    let rows = statement.query_map([], |row| {
        let entries: String = row.get(12)?;
        let embedding: Option<Vec<u8>> = row.get(13)?;
        Ok(Chunk { value: json!({
            "chunk_id":row.get::<_,String>(0)?, "document_id":row.get::<_,String>(1)?,
            "source_file_id":row.get::<_,String>(2)?, "corpus_file_id":row.get::<_,String>(3)?,
            "filename":row.get::<_,String>(4)?, "category":row.get::<_,String>(5)?,
            "line_start":row.get::<_,u64>(8)?, "line_end":row.get::<_,u64>(9)?,
            "date_start":row.get::<_,String>(10)?, "date_end":row.get::<_,String>(11)?,
            "entry_ids":serde_json::from_str::<Value>(&entries).unwrap_or(json!([])),
        }), content: row.get(6)?, normalized:row.get(7)?,
            vector: embedding.map(|bytes| bytes.chunks_exact(4).map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())).collect()),
        })
    }).map_err(|_| "source_index_read_failed")?;
    let mut result = Vec::new();
    for row in rows {
        let row = row.map_err(|_| "source_index_read_failed")?;
        if row.value["source_file_id"] != body["descriptor"]["source_file_id"]
            || row.value["corpus_file_id"] != body["corpus_file_id"]
        {
            return Err("source_index_read_failed");
        }
        result.push(row);
    }
    Ok(result)
}

fn normalize(text: &str) -> String {
    text.nfkc()
        .case_fold()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn snippet(content: &str, query: &str) -> String {
    let text = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= 420 {
        return text;
    }
    let pos = normalize(&text)
        .find(&normalize(query))
        .map(|byte| normalize(&text)[..byte].chars().count())
        .unwrap_or(0);
    let start = pos.saturating_sub(140).min(chars.len().saturating_sub(420));
    let end = (start + 420).min(chars.len());
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        chars[start..end].iter().collect::<String>().trim(),
        if end < chars.len() { "…" } else { "" }
    )
}

pub fn execute(root: &Path, user_id: &str, device_id: &str, body: &Value) -> SearchResult<Value> {
    if body["operation"]
        .as_str()
        .unwrap_or("")
        .starts_with("embedding_")
    {
        return enrich(root, user_id, device_id, body);
    }
    let index = open_index(root, user_id, device_id, body)?;
    let connection = &index.connection;
    let operation = string(body, "operation")?;
    if operation == "probe" {
        return Ok(json!({"ready":true}));
    }
    let mut rows = chunks(connection, body)?;
    if operation == "passage" {
        let id = string(&body["payload"], "chunk_id")?;
        if id.len() != 32 || !id.chars().all(|ch| ch.is_ascii_hexdigit()) {
            return Err("source_index_read_failed");
        }
        if let Some(row) = rows.into_iter().find(|row| row.value["chunk_id"] == id) {
            let mut value = row.value;
            value.as_object_mut().unwrap().remove("document_id");
            value["content"] = json!(row.content.chars().take(12000).collect::<String>());
            return Ok(value);
        }
        return Ok(json!({"not_found":true}));
    }
    if operation != "search" {
        return Err("source_index_read_failed");
    }
    let query = &body["payload"]["query"];
    let text = string(query, "text")?;
    if text.is_empty() || text.chars().count() > 512 {
        return Err("source_index_read_failed");
    }
    let instrument = string(query, "instrument")?;
    if !["keyword", "grep", "regex", "semantic", "hybrid"].contains(&instrument) {
        return Err("source_index_read_failed");
    }
    let lower = query["date_from"].as_str().unwrap_or("");
    let upper = query["date_to"].as_str().unwrap_or("");
    rows.retain(|row| {
        let start = row.value["date_start"].as_str().unwrap_or("");
        let end = row.value["date_end"].as_str().unwrap_or("");
        (lower.is_empty() || end.is_empty() || end >= lower)
            && (upper.is_empty() || start.is_empty() || start <= upper)
    });
    let terms = regex::Regex::new(r"[^\W_]+")
        .unwrap()
        .find_iter(&normalize(text))
        .map(|value| value.as_str().to_string())
        .collect::<Vec<_>>();
    let dimensions = body["descriptor"]["embedding_dimensions"]
        .as_u64()
        .unwrap_or(0) as usize;
    let vector = body["payload"]["vector"]
        .as_array()
        .and_then(|values| values.iter().map(Value::as_f64).collect::<Option<Vec<_>>>());
    let semantic = ["semantic", "hybrid"].contains(&instrument)
        && vector.as_ref().is_some_and(|vector| {
            dimensions > 0 && vector.len() == dimensions && vector.iter().all(|x| x.is_finite())
        });
    let lexical_fallback = instrument == "semantic"
        && (!semantic
            || !rows.iter().any(|row| {
                row.vector.as_ref().is_some_and(|stored| {
                    stored.len() == dimensions && stored.iter().all(|x| x.is_finite())
                })
            }));
    let mut rankings: Vec<(&str, Vec<(String, f64)>)> = Vec::new();
    if ["keyword", "hybrid"].contains(&instrument) || lexical_fallback {
        let fts = terms
            .iter()
            .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut ranked = Vec::new();
        if !fts.is_empty() {
            let mut statement = connection.prepare("select chunk_id,bm25(chunks_fts,1.0,0.35,0.2) from chunks_fts where chunks_fts match ? order by 2")
                .map_err(|_| "source_index_read_failed")?;
            for row in statement
                .query_map([fts], |row| {
                    Ok((row.get::<_, String>(0)?, -row.get::<_, f64>(1)?))
                })
                .map_err(|_| "source_index_read_failed")?
            {
                let value = row.map_err(|_| "source_index_read_failed")?;
                if rows.iter().any(|row| row.value["chunk_id"] == value.0) {
                    ranked.push(value);
                }
            }
        }
        rankings.push(("keyword", ranked));
    }
    if ["grep", "hybrid"].contains(&instrument) || lexical_fallback {
        let phrase = normalize(text);
        let mut ranked = rows
            .iter()
            .filter_map(|row| {
                let score = (if row.normalized.contains(&phrase) {
                    5.0
                } else {
                    0.0
                }) + (if normalize(row.value["filename"].as_str().unwrap_or(""))
                    .contains(&phrase)
                {
                    3.0
                } else {
                    0.0
                }) + terms
                    .iter()
                    .filter(|term| row.normalized.contains(term.as_str()))
                    .count() as f64;
                (score > 0.0).then(|| (row.value["chunk_id"].as_str().unwrap().to_string(), score))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        rankings.push(("grep", ranked));
    }
    if instrument == "regex" {
        if text.chars().count() > 256
            || text.contains(['(', ')', '|', '{', '}'])
            || regex::Regex::new(r"\\[1-9]|(?:\.\*){2,}|(?:\.\+){2,}|[*+?]\s*[*+?]")
                .unwrap()
                .is_match(text)
        {
            return Err("source_index_read_failed");
        }
        let pattern = regex::RegexBuilder::new(text)
            .case_insensitive(true)
            .build()
            .map_err(|_| "source_index_read_failed")?;
        let mut ranked = rows
            .iter()
            .filter_map(|row| {
                pattern.find(&row.content).map(|found| {
                    (
                        row.value["chunk_id"].as_str().unwrap().to_string(),
                        -(found.start() as f64),
                    )
                })
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        rankings.push(("regex", ranked));
    }
    let mut cosines = HashMap::new();
    if semantic {
        let vector = vector.unwrap();
        let norm = vector.iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm <= 0.0 {
            return Err("source_index_read_failed");
        }
        let mut ranked = rows
            .iter()
            .filter_map(|row| {
                let stored = row.vector.as_ref()?;
                if stored.len() != dimensions || stored.iter().any(|x| !x.is_finite()) {
                    return None;
                }
                let score = vector
                    .iter()
                    .zip(stored)
                    .map(|(a, b)| a / norm * (*b as f64))
                    .sum::<f64>();
                let id = row.value["chunk_id"].as_str().unwrap().to_string();
                cosines.insert(id.clone(), score);
                Some((id, score))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        rankings.push(("semantic", ranked));
    }
    let mut scores: HashMap<String, (f64, Vec<&str>)> = HashMap::new();
    for (method, ranking) in &rankings {
        let weight = match *method {
            "keyword" => 1.0,
            "semantic" => 1.2,
            "grep" => 1.1,
            _ => 1.2,
        };
        for (rank, (id, _)) in ranking.iter().enumerate() {
            let value = scores.entry(id.clone()).or_default();
            value.0 += weight / (61 + rank) as f64;
            value.1.push(method);
        }
    }
    let ceiling = scores
        .values()
        .map(|x| x.0)
        .fold(0.0, f64::max)
        .max(f64::MIN_POSITIVE);
    let mut ordered = scores
        .into_iter()
        .map(|(id, (score, mut methods))| {
            methods.sort_unstable();
            let score = if instrument == "semantic" && !cosines.is_empty() {
                cosines[&id]
            } else if instrument == "hybrid" && !cosines.is_empty() {
                0.7 * score / ceiling + 0.3 * cosines.get(&id).copied().unwrap_or(0.0).max(0.0)
            } else {
                score / ceiling
            };
            (id, score, methods)
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let limit = query["limit"].as_u64().ok_or("source_index_read_failed")? as usize;
    if limit > 0 {
        ordered.truncate(limit);
    }
    let hits = ordered
        .into_iter()
        .filter_map(|(id, score, methods)| {
            let row = rows.iter().find(|row| row.value["chunk_id"] == id)?;
            let mut value = row.value.clone();
            value["score"] = json!((score * 1e6).round() / 1e6);
            value["methods"] = json!(methods);
            value["snippet"] = json!(snippet(&row.content, text));
            Some(value)
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"hits":hits,"instrument":instrument,"semantic_available":semantic,
        "degraded_methods":if ["semantic","hybrid"].contains(&instrument) && !semantic {vec!["semantic"]} else {vec![]},
        "index_fingerprint":body["descriptor"]["source_fingerprint"]}),
    )
}

// The hosted relay authorizes a current job lease before forwarding these
// operations. The desktop independently binds its checkpoint to the original
// source digest, owner and device. An old text index remains immutable and
// searchable while the backend publishes verified checkpoint pointers.
fn enrich(root: &Path, user_id: &str, device_id: &str, body: &Value) -> SearchResult<Value> {
    let original = open_index(root, user_id, device_id, body)?;
    let job = string(&body["payload"], "job_id")?;
    uuid::Uuid::parse_str(job).map_err(|_| "source_index_read_failed")?;
    uuid::Uuid::parse_str(string(&body["payload"], "lease_owner")?)
        .map_err(|_| "source_index_read_failed")?;
    let path = format!(
        "{user_id}/source_search/checkpoints/{job}/{}.sqlite.gz",
        string(&body["descriptor"], "artifact_sha256")?
    );
    let (_, target) = super::resolve_storage_file(root, super::StorageBucket::Outputs, &path)
        .map_err(|_| "source_index_read_failed")?;
    let mut checkpoint_body = body.clone();
    let mut existing = None;
    if target.exists() {
        let size = std::fs::metadata(&target)
            .map_err(|_| "source_index_read_failed")?
            .len();
        if size == 0 || size > MAX_INDEX_BYTES {
            return Err("source_index_read_failed");
        }
        let bytes = std::fs::read(&target).map_err(|_| "source_index_read_failed")?;
        checkpoint_body["descriptor"]["storage_path"] = json!(path);
        checkpoint_body["descriptor"]["artifact_size_bytes"] = json!(size);
        checkpoint_body["descriptor"]["artifact_sha256"] =
            json!(format!("{:x}", Sha256::digest(&bytes)));
        checkpoint_body["descriptor"]["embedding_provider"] = json!("openai");
        checkpoint_body["descriptor"]["embedding_model"] = json!("text-embedding-3-small");
        checkpoint_body["descriptor"]["embedding_dimensions"] = json!(768);
        existing = Some(open_index(root, user_id, device_id, &checkpoint_body)?);
    }
    let index = existing.as_ref().unwrap_or(&original);
    let pending: Vec<(String, String)> = index
        .connection
        .prepare(
            "select chunk_id,content from chunks where embedding is null order by chunk_id limit 4",
        )
        .map_err(|_| "source_index_read_failed")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(|_| "source_index_read_failed")?
        .collect::<Result<_, _>>()
        .map_err(|_| "source_index_read_failed")?;
    if body["operation"] == "embedding_batch" {
        let proof = if pending.is_empty() && existing.is_some() {
            json!({"checkpoint_path":path,
              "checkpoint_sha256":checkpoint_body["descriptor"]["artifact_sha256"],
              "checkpoint_size":checkpoint_body["descriptor"]["artifact_size_bytes"]})
        } else {
            json!({})
        };
        let complete = pending.is_empty();
        let mut result = json!({"batch":pending.into_iter().map(|(id,content)|
            json!({"chunk_id":id,"content":content})).collect::<Vec<_>>(),"complete":complete});
        result
            .as_object_mut()
            .unwrap()
            .extend(proof.as_object().unwrap().clone());
        return Ok(result);
    }
    if body["operation"] != "embedding_commit" {
        return Err("source_index_read_failed");
    }
    let ids = body["payload"]["chunk_ids"]
        .as_array()
        .ok_or("source_index_read_failed")?;
    let hex = string(&body["payload"], "vectors_hex")?;
    if ids.is_empty()
        || ids.len() != pending.len()
        || hex.len() != ids.len() * 768 * 8
        || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        || ids
            .iter()
            .zip(&pending)
            .any(|(id, (expected, _))| id.as_str() != Some(expected))
    {
        return Err("source_index_read_failed");
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "source_index_read_failed")?;
    let mut connection =
        Connection::open(index._file.path()).map_err(|_| "source_index_read_failed")?;
    connection
        .execute_batch("pragma trusted_schema=off; pragma journal_mode=DELETE;")
        .map_err(|_| "source_index_read_failed")?;
    let tx = connection
        .transaction()
        .map_err(|_| "source_index_read_failed")?;
    for (id, vector) in ids.iter().zip(bytes.chunks_exact(768 * 4)) {
        let values: Vec<f32> = vector
            .chunks_exact(4)
            .map(|v| f32::from_le_bytes(v.try_into().unwrap()))
            .collect();
        let norm: f32 = values.iter().map(|v| v * v).sum::<f32>();
        if values.iter().any(|v| !v.is_finite()) || (norm - 1.0).abs() > 0.01 {
            return Err("source_index_read_failed");
        }
        if tx
            .execute(
                "update chunks set embedding=? where chunk_id=? and embedding is null",
                rusqlite::params![vector, id.as_str().unwrap()],
            )
            .map_err(|_| "source_index_read_failed")?
            != 1
        {
            return Err("source_index_read_failed");
        }
    }
    for (key, value) in [
        ("embedding_provider", "openai"),
        ("embedding_model", "text-embedding-3-small"),
        ("embedding_dimensions", "768"),
    ] {
        tx.execute(
            "update index_metadata set value=? where key=?",
            [value, key],
        )
        .map_err(|_| "source_index_read_failed")?;
    }
    let remaining: i64 = tx
        .query_row(
            "select count(*) from chunks where embedding is null",
            [],
            |r| r.get(0),
        )
        .map_err(|_| "source_index_read_failed")?;
    tx.commit().map_err(|_| "source_index_read_failed")?;
    drop(connection);
    let raw = std::fs::read(index._file.path()).map_err(|_| "source_index_read_failed")?;
    if raw.len() as u64 > MAX_INDEX_BYTES {
        return Err("source_index_read_failed");
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(&raw)
        .map_err(|_| "source_index_read_failed")?;
    let output = encoder.finish().map_err(|_| "source_index_read_failed")?;
    let parent = target.parent().ok_or("source_index_read_failed")?;
    std::fs::create_dir_all(parent).map_err(|_| "source_index_read_failed")?;
    super::resolve_storage_file(root, super::StorageBucket::Outputs, &path)
        .map_err(|_| "source_index_read_failed")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_| "source_index_read_failed")?;
    temporary
        .write_all(&output)
        .map_err(|_| "source_index_read_failed")?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|_| "source_index_read_failed")?;
    temporary
        .persist(&target)
        .map_err(|_| "source_index_read_failed")?;
    Ok(
        json!({"checkpoint_path":path,"checkpoint_sha256":format!("{:x}",Sha256::digest(&output)),
              "checkpoint_size":output.len(),"complete":remaining==0}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    const OWNER: &str = "00000000-0000-0000-0000-000000000001";
    const DEVICE: &str = "00000000-0000-0000-0000-000000000002";
    const SOURCE: &str = "00000000-0000-0000-0000-000000000003";
    const CORPUS: &str = "00000000-0000-0000-0000-000000000004";

    fn fixture() -> (tempfile::TempDir, Value, Vec<u8>) {
        let root = tempfile::tempdir().unwrap();
        let database = tempfile::NamedTempFile::new().unwrap();
        let connection = Connection::open(database.path()).unwrap();
        connection.execute_batch("create table index_metadata(key text,value text);
          create table documents(document_id text,source_file_id text,corpus_file_id text,filename text,category text);
          create table chunks(chunk_id text,document_id text,content text,normalized_content text,
            line_start integer,line_end integer,date_start text,date_end text,entry_ids_json text,embedding blob);
          create virtual table chunks_fts using fts5(chunk_id unindexed,content,filename,category);").unwrap();
        let path = format!("{OWNER}/source_search/files/{SOURCE}/v1-test.sqlite.gz");
        let mut descriptor = json!({"user_id":OWNER,"source_file_id":SOURCE,"storage_mode":"local",
            "storage_device_id":DEVICE,"storage_path":path,"index_format_version":1,"status":"ready",
            "source_fingerprint":"synthetic-fingerprint","embedding_provider":"test","embedding_model":"test-vector",
            "embedding_dimensions":2,"document_count":1,"chunk_count":3});
        for (key, value) in [
            ("format_version", "1"),
            ("user_id", OWNER),
            ("source_file_id", SOURCE),
            ("source_fingerprint", "synthetic-fingerprint"),
            ("embedding_provider", "test"),
            ("embedding_model", "test-vector"),
            ("embedding_dimensions", "2"),
            ("document_count", "1"),
            ("chunk_count", "3"),
        ] {
            connection
                .execute("insert into index_metadata values (?,?)", [key, value])
                .unwrap();
        }
        connection
            .execute(
                "insert into documents values (?,?,?,?,?)",
                rusqlite::params![
                    format!("source:{SOURCE}"),
                    SOURCE,
                    CORPUS,
                    "synthetic.txt",
                    "document"
                ],
            )
            .unwrap();
        for (number, text, vector) in [
            (
                1,
                "Jardín y 園芸 园艺. I enjoy quiet gardening and Straße walks.",
                [1.0f32, 0.0],
            ),
            (2, "Ocean sailing is different.", [0.0, 1.0]),
            (3, "Garden design and planting at home.", [1.0, 0.0]),
        ] {
            let id = format!("{number:032x}");
            let blob = vector
                .iter()
                .flat_map(|x| x.to_le_bytes())
                .collect::<Vec<_>>();
            connection
                .execute(
                    "insert into chunks values (?,?,?,?,?,?,?,?,?,?)",
                    rusqlite::params![
                        id,
                        format!("source:{SOURCE}"),
                        text,
                        normalize(text),
                        number,
                        number,
                        "2026-01-01",
                        "2026-01-01",
                        format!("[\"record-{number}\"]"),
                        blob
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "insert into chunks_fts values (?,?,?,?)",
                    [id.as_str(), text, "synthetic.txt", "document"],
                )
                .unwrap();
        }
        drop(connection);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&std::fs::read(database.path()).unwrap())
            .unwrap();
        let artifact = encoder.finish().unwrap();
        let destination = root.path().join("outputs").join(&path);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, &artifact).unwrap();
        descriptor["artifact_sha256"] = json!(format!("{:x}", Sha256::digest(&artifact)));
        descriptor["artifact_size_bytes"] = json!(artifact.len());
        (
            root,
            json!({"descriptor":descriptor,"corpus_file_id":CORPUS,"operation":"search","payload":{
            "query":{"text":"garden","instrument":"grep","limit":0,"date_from":"","date_to":""},"vector":null}}),
            artifact,
        )
    }

    fn lexical_fixture() -> (tempfile::TempDir, Value, Vec<u8>) {
        let (root, mut body, _) = fixture();
        let reader = open_index(root.path(), OWNER, DEVICE, &body).unwrap();
        let connection = Connection::open(reader._file.path()).unwrap();
        connection.execute_batch("update chunks set embedding=null;
            update index_metadata set value='' where key in ('embedding_provider','embedding_model');
            update index_metadata set value='0' where key='embedding_dimensions';").unwrap();
        drop(connection);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&std::fs::read(reader._file.path()).unwrap())
            .unwrap();
        let artifact = encoder.finish().unwrap();
        let path = root
            .path()
            .join("outputs")
            .join(body["descriptor"]["storage_path"].as_str().unwrap());
        std::fs::write(path, &artifact).unwrap();
        body["descriptor"]["artifact_sha256"] = json!(format!("{:x}", Sha256::digest(&artifact)));
        body["descriptor"]["artifact_size_bytes"] = json!(artifact.len());
        body["descriptor"]["embedding_provider"] = json!("");
        body["descriptor"]["embedding_model"] = json!("");
        body["descriptor"]["embedding_dimensions"] = json!(0);
        body["payload"] = json!({"job_id":"00000000-0000-0000-0000-000000000005",
            "lease_owner":"00000000-0000-0000-0000-000000000006"});
        body["operation"] = json!("embedding_batch");
        (root, body, artifact)
    }

    #[test]
    fn local_embedding_checkpoint_recovers_without_mutating_the_text_index() {
        let (root, mut body, original) = lexical_fixture();
        let first = execute(root.path(), OWNER, DEVICE, &body).unwrap();
        assert_eq!(first["batch"].as_array().unwrap().len(), 3);
        let ids: Vec<Value> = first["batch"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["chunk_id"].clone())
            .collect();
        let mut vector = vec![0.0f32; 768];
        vector[0] = 1.0;
        let hex: String = vector
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .map(|b| format!("{b:02x}"))
            .collect();
        body["operation"] = json!("embedding_commit");
        body["payload"]["chunk_ids"] = json!(ids);
        body["payload"]["vectors_hex"] = json!(hex.repeat(3));
        let result = execute(root.path(), OWNER, DEVICE, &body).unwrap();
        assert_eq!(result["complete"], true);
        // Simulate losing the final acknowledgement and restarting the worker.
        body["operation"] = json!("embedding_batch");
        let recovered = execute(root.path(), OWNER, DEVICE, &body).unwrap();
        assert_eq!(recovered["complete"], true);
        assert_eq!(recovered["batch"], json!([]));
        assert_eq!(recovered["checkpoint_sha256"], result["checkpoint_sha256"]);
        assert_eq!(
            std::fs::read(
                root.path()
                    .join("outputs")
                    .join(body["descriptor"]["storage_path"].as_str().unwrap())
            )
            .unwrap(),
            original
        );
        body["operation"] = json!("embedding_commit");
        assert!(execute(root.path(), OWNER, DEVICE, &body).is_err()); // stale batch
    }

    #[test]
    fn local_embedding_mutations_reject_wrong_scope_and_invalid_vectors() {
        let (root, mut body, _) = lexical_fixture();
        assert!(execute(root.path(), "other-owner", DEVICE, &body).is_err());
        assert!(execute(root.path(), OWNER, "other-device", &body).is_err());
        let batch = execute(root.path(), OWNER, DEVICE, &body).unwrap();
        body["operation"] = json!("embedding_commit");
        body["payload"]["chunk_ids"] = json!(batch["batch"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["chunk_id"].clone())
            .collect::<Vec<_>>());
        body["payload"]["vectors_hex"] = json!("0".repeat(3 * 768 * 8));
        assert!(execute(root.path(), OWNER, DEVICE, &body).is_err()); // zero-magnitude vectors
        body["payload"]["vectors_hex"] = json!("z".repeat(3 * 768 * 8));
        assert!(execute(root.path(), OWNER, DEVICE, &body).is_err());
        body["payload"]["vectors_hex"] = json!("€".repeat(3 * 768 * 8 / 3));
        assert!(execute(root.path(), OWNER, DEVICE, &body).is_err()); // no UTF-8 slicing panic
        body["payload"]["job_id"] = json!("../../foreign");
        assert!(execute(root.path(), OWNER, DEVICE, &body).is_err());
    }

    #[test]
    fn local_reader_all_instruments_unicode_filters_passages_and_preservation() {
        let (root, mut body, artifact) = fixture();
        for (instrument, text, vector) in [
            ("keyword", "Jardín", Value::Null),
            ("grep", "straße", Value::Null),
            ("regex", "Jard.n", Value::Null),
            (
                "semantic",
                "a multilingual horticulture question",
                json!([1.0, 0.0]),
            ),
            ("hybrid", "garden", json!([1.0, 0.0])),
        ] {
            body["payload"]["query"]["instrument"] = json!(instrument);
            body["payload"]["query"]["text"] = json!(text);
            body["payload"]["vector"] = vector;
            let result = execute(root.path(), OWNER, DEVICE, &body).unwrap();
            assert!(
                !result["hits"].as_array().unwrap().is_empty(),
                "{instrument}"
            );
            assert_eq!(result["instrument"], instrument);
            for hit in result["hits"].as_array().unwrap() {
                assert_eq!(hit["source_file_id"], SOURCE);
                let mut fetch = body.clone();
                fetch["operation"] = json!("passage");
                fetch["payload"] = json!({"chunk_id":hit["chunk_id"]});
                let passage = execute(root.path(), OWNER, DEVICE, &fetch).unwrap();
                assert!(!passage["content"].as_str().unwrap().is_empty());
                assert_eq!(passage["entry_ids"], hit["entry_ids"]);
            }
        }
        body["payload"]["vector"] = Value::Null;
        body["payload"]["query"]["instrument"] = json!("semantic");
        let fallback = execute(root.path(), OWNER, DEVICE, &body).unwrap();
        assert_eq!(fallback["semantic_available"], false);
        assert!(!fallback["hits"].as_array().unwrap().is_empty());
        body["payload"]["query"]["date_from"] = json!("2027-01-01");
        assert!(execute(root.path(), OWNER, DEVICE, &body).unwrap()["hits"]
            .as_array()
            .unwrap()
            .is_empty());
        body["operation"] = json!("passage");
        body["payload"] = json!({"chunk_id":"ffffffffffffffffffffffffffffffff"});
        assert_eq!(
            execute(root.path(), OWNER, DEVICE, &body).unwrap()["not_found"],
            true
        );
        let path = root
            .path()
            .join("outputs")
            .join(body["descriptor"]["storage_path"].as_str().unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), artifact);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "temporary reader copies are removed"
        );
    }

    #[test]
    fn index_identity_digest_path_device_and_corpus_are_fenced() {
        let (root, body, _) = fixture();
        for (field, value) in [
            ("user_id", json!(DEVICE)),
            ("source_file_id", json!(DEVICE)),
            ("storage_device_id", json!(OWNER)),
            ("storage_mode", json!("cloud")),
            ("status", json!("building")),
            ("index_format_version", json!(2)),
            ("artifact_sha256", json!("0".repeat(64))),
            ("artifact_size_bytes", json!(1)),
            ("storage_path", json!(format!("{OWNER}/../escape"))),
        ] {
            let mut wrong = body.clone();
            wrong["descriptor"][field] = value;
            assert!(
                execute(root.path(), OWNER, DEVICE, &wrong).is_err(),
                "{field}"
            );
        }
        let mut wrong = body.clone();
        wrong["corpus_file_id"] = json!(DEVICE);
        assert!(execute(root.path(), OWNER, DEVICE, &wrong).is_err());
        assert!(execute(root.path(), DEVICE, DEVICE, &body).is_err());
        assert!(execute(root.path(), OWNER, OWNER, &body).is_err());
    }

    #[test]
    fn unsafe_queries_are_rejected_and_probe_is_read_only() {
        let (root, mut body, _) = fixture();
        body["operation"] = json!("probe");
        assert_eq!(
            execute(root.path(), OWNER, DEVICE, &body).unwrap()["ready"],
            true
        );
        body["operation"] = json!("search");
        body["payload"]["query"]["instrument"] = json!("regex");
        for pattern in ["(a+)+", "a|b", ".*.*", "\\1", "a{5}"] {
            body["payload"]["query"]["text"] = json!(pattern);
            assert!(execute(root.path(), OWNER, DEVICE, &body).is_err());
        }
        body["payload"]["query"]["text"] = json!("a".repeat(513));
        assert!(execute(root.path(), OWNER, DEVICE, &body).is_err());
    }
}
