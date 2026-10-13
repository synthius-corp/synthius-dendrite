//! Durable content checkpoints contain source data only, never session material.
use crate::protocol::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub file_id: Uuid,
    pub stored_filename: String,
    pub original_filename: String,
    pub storage_path: String,
    pub size: usize,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub version: u32,
    pub job_id: Uuid,
    pub context: Context,
    pub source_usage: String,
    pub account_id: Option<i64>,
    pub scope: Option<Scope>,
    pub cursors: Vec<Cursor>,
    pub pages: Vec<String>,
    pub complete: bool,
    pub registered: Vec<Uuid>,
    pub sources: Vec<Source>,
    /// Source files whose deterministic descriptors were durably recorded
    /// before materialization. This closes the crash window between writing a
    /// local source and publishing it in `sources`.
    #[serde(default)]
    pub pending_sources: Vec<Source>,
    pub logout_unconfirmed: bool,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub consented_at: Option<i64>,
}
impl Journal {
    pub fn new(context: Context, usage: String) -> Result<Self, &'static str> {
        if !matches!(
            usage.as_str(),
            "context_only" | "persona_source_and_context"
        ) {
            return Err("invalid_usage");
        }
        Ok(Self {
            version: 1,
            job_id: Uuid::new_v4(),
            context,
            source_usage: usage,
            account_id: None,
            scope: None,
            cursors: vec![],
            pages: vec![],
            complete: false,
            registered: vec![],
            sources: vec![],
            pending_sources: vec![],
            logout_unconfirmed: false,
            cancelled: false,
            consented_at: None,
        })
    }
    pub fn folder(&self, root: &Path) -> PathBuf {
        root.join("metadata")
            .join("telegram-imports")
            .join(self.context.user_id.to_string())
            .join(self.job_id.to_string())
    }
    pub fn save(&self, root: &Path) -> Result<(), &'static str> {
        atomic_write(
            root,
            &self.folder(root).join("journal.json"),
            &serde_json::to_vec(self).map_err(|_| "journal_invalid")?,
            false,
        )
    }
    pub fn validate_metadata(
        &self,
        expected: &Context,
        expected_job: Uuid,
    ) -> Result<(), &'static str> {
        let now = chrono::Utc::now().timestamp();
        if self.version != 1
            || self.job_id != expected_job
            || self.context.user_id != expected.user_id
            || self.context.persona_id != expected.persona_id
            || self.context.device_id != expected.device_id
            || self.context.persona_name.len() > 256
            || !matches!(
                self.source_usage.as_str(),
                "context_only" | "persona_source_and_context"
            )
            || self.account_id.is_some_and(|id| id <= 0)
            || self.scope.is_some() && self.account_id.is_none()
            || self.scope.is_some() != self.consented_at.is_some()
            || self
                .consented_at
                .is_some_and(|time| time <= 0 || time > now + 300)
            || self
                .pages
                .iter()
                .any(|id| id.len() != 64 || !id.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err("journal_invalid");
        }
        if let Some(scope) = &self.scope {
            scope.validate(now).map_err(|_| "journal_invalid")?;
            let mut cursors = std::collections::HashSet::new();
            if self.cursors.len() > scope.chats.len()
                || self.cursors.iter().any(|cursor| {
                    cursor.offset_id < 0
                        || cursor.high_water < 0
                        || !cursors.insert((cursor.chat_id, cursor.topic_id))
                        || !scope.chats.iter().any(|selection| {
                            selection.chat_id == cursor.chat_id
                                && selection.topic_id == cursor.topic_id
                        })
                })
            {
                return Err("journal_invalid");
            }
        } else if !self.cursors.is_empty() || !self.pages.is_empty() {
            return Err("journal_invalid");
        }
        let page_ids = self.pages.iter().collect::<std::collections::HashSet<_>>();
        if page_ids.len() != self.pages.len() {
            return Err("journal_invalid");
        }
        let mut source_ids = std::collections::HashSet::new();
        for source in self.sources.iter().chain(&self.pending_sources) {
            if !source_ids.insert(source.file_id)
                || source.stored_filename != format!("{}.json", source.file_id)
                || source.storage_path
                    != format!(
                        "{}/{}/{}",
                        expected.user_id, source.file_id, source.stored_filename
                    )
                || source.original_filename.is_empty()
                || source.original_filename.len() > 255
                || Path::new(&source.original_filename)
                    .file_name()
                    .and_then(|v| v.to_str())
                    != Some(source.original_filename.as_str())
                || source.size == 0
                || source.size > MAX_SOURCE
                || source.sha256.len() != 64
                || !source
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
            {
                return Err("journal_invalid");
            }
        }
        let registered = self
            .registered
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let published = self
            .sources
            .iter()
            .map(|source| source.file_id)
            .collect::<std::collections::HashSet<_>>();
        if registered.len() != self.registered.len()
            || registered.iter().any(|id| !published.contains(id))
            || (self.complete && !self.pending_sources.is_empty())
            || (!self.complete && (!self.sources.is_empty() || !self.registered.is_empty()))
            || (self.cancelled && (self.complete || !self.registered.is_empty()))
        {
            return Err("journal_invalid");
        }
        Ok(())
    }
    pub fn begin(&mut self, scope: Scope, account: i64) -> Result<(), &'static str> {
        if self.cancelled {
            return Err("import_cancelled");
        }
        scope.validate(chrono::Utc::now().timestamp())?;
        if account <= 0
            || self.account_id.is_some_and(|id| id != account)
            || self.scope.as_ref().is_some_and(|old| old != &scope)
        {
            return Err("resume_identity_mismatch");
        }
        self.account_id = Some(account);
        self.scope = Some(scope);
        self.logout_unconfirmed = true;
        // Resume preserves the original consent time; it is not a new grant.
        self.consented_at
            .get_or_insert_with(|| chrono::Utc::now().timestamp());
        Ok(())
    }
    pub fn commit_page(&mut self, root: &Path, event: &Event) -> Result<(), &'static str> {
        let Event::Page {
            page_id,
            account_id,
            chat_id,
            title,
            cursor,
            messages,
        } = event
        else {
            return Err("invalid_page");
        };
        if page_id.len() != 64
            || !page_id.chars().all(|c| c.is_ascii_hexdigit())
            || messages.len() > 101
            || title.len() > 1024
            || Some(*account_id) != self.account_id
            || cursor.chat_id != *chat_id
            || cursor.offset_id < 0
            || cursor.high_water < 0
        {
            return Err("invalid_page");
        }
        let scope = self.scope.as_ref().ok_or("invalid_page")?;
        let selection = scope
            .chats
            .iter()
            .find(|s| s.chat_id == *chat_id && s.topic_id == cursor.topic_id)
            .ok_or("invalid_page")?;
        if messages
            .iter()
            .any(|m| !m.validate(scope, selection) || m.id > cursor.high_water)
            || *page_id != page_digest(*account_id, selection, cursor, messages)
        {
            return Err("invalid_page");
        }
        let old = self
            .cursors
            .iter()
            .find(|c| c.chat_id == cursor.chat_id && c.topic_id == cursor.topic_id);
        if !self.pages.contains(page_id)
            && old.is_some_and(|c| {
                c.done
                    || (c.offset_id > 0 && cursor.offset_id >= c.offset_id)
                    || c.high_water != cursor.high_water
                    || cursor.pages != c.pages.saturating_add(1)
            })
        {
            return Err("cursor_not_advancing");
        }
        if !self.pages.contains(page_id) && old.is_none() && cursor.pages != 1 {
            return Err("cursor_not_advancing");
        }
        let bytes = serde_json::to_vec(event).map_err(|_| "invalid_page")?;
        if bytes.len() > MAX_FRAME {
            return Err("invalid_page");
        }
        // Commit the page before advancing its cursor or acknowledging it.
        atomic_write(
            root,
            &self.folder(root).join(format!("{page_id}.json")),
            &bytes,
            true,
        )?;
        if !self.pages.contains(page_id) {
            self.pages.push(page_id.clone());
            self.cursors
                .retain(|c| c.chat_id != cursor.chat_id || c.topic_id != cursor.topic_id);
            self.cursors.push(cursor.clone());
        }
        self.save(root)
    }
    pub fn finish(&mut self, root: &Path) -> Result<(), &'static str> {
        if self.complete {
            return if self.pending_sources.is_empty() {
                Ok(())
            } else {
                Err("journal_invalid")
            };
        }
        let scope = self.scope.clone().ok_or("missing_scope")?;
        let account_id = self.account_id.ok_or("missing_account")?;
        if scope.chats.iter().any(|s| {
            !self
                .cursors
                .iter()
                .any(|c| c.chat_id == s.chat_id && c.topic_id == s.topic_id && c.done)
        }) {
            return Err("incomplete_history");
        }
        let mut sources = vec![];
        let mut index: std::collections::BTreeMap<(i64, Option<i32>), Vec<String>> =
            Default::default();
        let mut page_cursors = Vec::new();
        // Validate all pages once, then read only each selection's own pages.
        for page_id in &self.pages {
            let Event::Page {
                chat_id, cursor, ..
            } = self.read_page(root, page_id)?
            else {
                return Err("checkpoint_invalid");
            };
            index
                .entry((chat_id, cursor.topic_id))
                .or_default()
                .push(page_id.clone());
            page_cursors.push(cursor);
        }
        if self
            .cursors
            .iter()
            .any(|cursor| !page_cursors.contains(cursor))
        {
            return Err("checkpoint_invalid");
        }
        // Materialize one selection at a time. Bound both disk input and peak RAM.
        for selected in &scope.chats {
            let mut title = String::new();
            let mut messages = std::collections::BTreeMap::new();
            for page_id in index
                .get(&(selected.chat_id, selected.topic_id))
                .into_iter()
                .flatten()
            {
                let Event::Page {
                    chat_id,
                    title: label,
                    cursor,
                    messages: page,
                    ..
                } = self.read_page(root, page_id)?
                else {
                    return Err("checkpoint_invalid");
                };
                if chat_id != selected.chat_id || cursor.topic_id != selected.topic_id {
                    continue;
                }
                title = label;
                for message in page {
                    messages.insert(message.id, message);
                }
            }
            let chat_id = selected.chat_id;
            let topic = selected.topic_id;
            let mut partition = vec![];
            let mut part = 0;
            let source_overhead = serde_json::to_vec(
                &json!({"name":&title,"type":"telegram_snapshot","id":chat_id,"messages":[]}),
            )
            .map_err(|_| "source_invalid")?
            .len();
            let mut size = source_overhead;
            for message in messages.into_values() {
                let value = json!({"id":format!("tg:{account_id}:{chat_id}:{}",message.id),"type":"message",
                    "date":chrono::DateTime::from_timestamp(i64::from(message.date),0).ok_or("invalid_date")?.to_rfc3339(), "from":message.author,"from_id":message.author_id,"text":message.text,
                    "synthius_telegram":{"version":1,"account_id":account_id,"chat_id":chat_id,"message_id":message.id,"topic_id":message.topic_id,"reply_to":message.reply_to,"edited_at":message.edited_at,"forwarded_from":message.forwarded_from,"forwarded_date":message.forwarded_date,"self_authored":message.self_authored}});
                let len = serde_json::to_vec(&value)
                    .map_err(|_| "source_invalid")?
                    .len();
                let delimiter = usize::from(!partition.is_empty());
                if size.saturating_add(delimiter).saturating_add(len) > MAX_SOURCE
                    && !partition.is_empty()
                {
                    sources.push(
                        self.materialize_source(root, chat_id, topic, &title, part, &partition)?,
                    );
                    part += 1;
                    partition.clear();
                    size = source_overhead;
                }
                size = size
                    .saturating_add(usize::from(!partition.is_empty()))
                    .saturating_add(len);
                partition.push(value);
            }
            if !partition.is_empty() {
                sources
                    .push(self.materialize_source(root, chat_id, topic, &title, part, &partition)?);
            }
        }
        if self
            .pending_sources
            .iter()
            .any(|pending| !sources.contains(pending))
        {
            return Err("source_invalid");
        }
        self.sources = sources;
        self.pending_sources.clear();
        self.complete = true;
        self.save(root)?;
        // Checkpoints are redundant once the final immutable sources and their
        // proofs are durable. A later catalogue read retries any OS-level
        // cleanup failure using the still-recorded page list.
        let _ = self.clear_checkpoints(root);
        Ok(())
    }
    pub fn clear_checkpoints(&mut self, root: &Path) -> Result<(), &'static str> {
        for id in &self.pages {
            let path = self.folder(root).join(format!("{id}.json"));
            ensure_safe(root, &path)?;
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("storage_unavailable"),
            }
        }
        self.pages.clear();
        self.save(root)
    }
    pub fn discard(&mut self, root: &Path) -> Result<(), &'static str> {
        if self.complete || !self.registered.is_empty() {
            return Err("completed_source_preserved");
        }
        // Completed source files remain ordinary local files. A cancelled job
        // owns unpublished checkpoints and only those materialized files whose
        // exact descriptor was durably recorded before their bytes were written.
        self.cancelled = true;
        self.save(root)?;
        for source in &self.pending_sources {
            let path = root.join("uploads").join(&source.storage_path);
            ensure_safe(root, &path)?;
            match fs::metadata(&path) {
                Ok(metadata) if metadata.len() == source.size as u64 => {
                    let bytes = fs::read(&path).map_err(|_| "storage_unavailable")?;
                    if digest(&bytes) != source.sha256 {
                        return Err("source_collision");
                    }
                    fs::remove_file(&path).map_err(|_| "storage_unavailable")?;
                }
                Ok(_) => return Err("source_collision"),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err("storage_unavailable"),
            }
        }
        self.pending_sources.clear();
        self.save(root)?;
        self.clear_checkpoints(root)
    }
    fn read_page(&self, root: &Path, id: &str) -> Result<Event, &'static str> {
        let path = self.folder(root).join(format!("{id}.json"));
        ensure_safe(root, &path)?;
        let bytes = fs::read(path).map_err(|_| "checkpoint_unavailable")?;
        if bytes.len() > MAX_FRAME {
            return Err("checkpoint_invalid");
        }
        let event: Event = serde_json::from_slice(&bytes).map_err(|_| "checkpoint_invalid")?;
        let Event::Page {
            page_id,
            account_id,
            chat_id,
            title,
            cursor,
            messages,
        } = &event
        else {
            return Err("checkpoint_invalid");
        };
        let scope = self.scope.as_ref().ok_or("missing_scope")?;
        let selected = scope
            .chats
            .iter()
            .find(|s| s.chat_id == *chat_id && s.topic_id == cursor.topic_id)
            .ok_or("checkpoint_invalid")?;
        if page_id != id
            || Some(*account_id) != self.account_id
            || cursor.chat_id != *chat_id
            || cursor.offset_id < 0
            || cursor.high_water < 0
            || title.len() > 1024
            || messages.len() > 101
            || messages
                .iter()
                .any(|m| !m.validate(scope, selected) || m.id > cursor.high_water)
            || page_digest(*account_id, selected, cursor, messages) != id
        {
            return Err("checkpoint_invalid");
        }
        Ok(event)
    }
    fn materialize_source(
        &mut self,
        root: &Path,
        chat: i64,
        topic: Option<i32>,
        title: &str,
        part: usize,
        messages: &[serde_json::Value],
    ) -> Result<Source, &'static str> {
        let id = Uuid::new_v5(&self.job_id, format!("{chat}:{topic:?}:{part}").as_bytes());
        let stored = format!("{id}.json");
        let path = format!("{}/{id}/{stored}", self.context.user_id);
        let bytes = serde_json::to_vec(
            &json!({"name":title,"type":"telegram_snapshot","id":chat,"messages":messages}),
        )
        .map_err(|_| "source_invalid")?;
        if bytes.len() > MAX_SOURCE {
            // finish() partitions before calling this method; exceeding the
            // per-source bound is an internal invariant violation, not a user
            // selection limit.
            return Err("source_invalid");
        }
        let source = Source {
            file_id: id,
            stored_filename: stored,
            // Cloud registration sees the display filename even for a local
            // source, so never expose the Telegram chat identifier there.
            original_filename: format!("Telegram snapshot {}.json", &id.simple().to_string()[..12]),
            storage_path: path,
            size: bytes.len(),
            sha256: digest(&bytes),
        };
        match self
            .pending_sources
            .iter()
            .find(|pending| pending.file_id == source.file_id)
        {
            Some(pending) if pending != &source => return Err("source_collision"),
            Some(_) => {}
            None => {
                self.pending_sources.push(source.clone());
                // Persist ownership before the source bytes exist so a crash can
                // never leave an untracked plaintext file behind.
                self.save(root)?;
            }
        }
        atomic_write(
            root,
            &root.join("uploads").join(&source.storage_path),
            &bytes,
            true,
        )?;
        Ok(source)
    }
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn page_digest(
    account: i64,
    selection: &Selection,
    cursor: &Cursor,
    messages: &[Message],
) -> String {
    digest(&serde_json::to_vec(&(account, selection, cursor, messages)).expect("serializable page"))
}
pub fn ensure_safe(root: &Path, path: &Path) -> Result<(), &'static str> {
    if !path.starts_with(root)
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("unsafe_path");
    }
    let mut check = path.to_path_buf();
    loop {
        if let Ok(m) = fs::symlink_metadata(&check) {
            if m.file_type().is_symlink() {
                return Err("unsafe_path");
            }
        }
        if check == root {
            break;
        }
        if !check.pop() {
            return Err("unsafe_path");
        }
    }
    Ok(())
}
fn atomic_write(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    immutable: bool,
) -> Result<(), &'static str> {
    ensure_safe(root, path)?;
    fs::create_dir_all(path.parent().ok_or("unsafe_path")?).map_err(|_| "storage_unavailable")?;
    ensure_safe(root, path)?;
    if immutable && path.exists() {
        return if fs::read(path).map_err(|_| "storage_unavailable")? == bytes {
            Ok(())
        } else {
            Err("checkpoint_collision")
        };
    }
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())
        .map_err(|_| "storage_unavailable")?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| "storage_unavailable")?;
    if immutable {
        file.persist_noclobber(path)
            .map_err(|_| "storage_unavailable")?;
    } else {
        file.persist(path).map_err(|_| "storage_unavailable")?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Journal) {
        let root = tempfile::tempdir().unwrap();
        let mut j = Journal::new(
            Context {
                user_id: Uuid::new_v4(),
                persona_id: Uuid::new_v4(),
                device_id: Uuid::new_v4(),
                persona_name: "Synthetic".into(),
            },
            "context_only".into(),
        )
        .unwrap();
        j.begin(
            Scope {
                chats: vec![Selection {
                    chat_id: 42,
                    topic_id: None,
                }],
                from: 1,
                until: 200,
            },
            7,
        )
        .unwrap();
        (root, j)
    }
    fn page() -> Event {
        let cursor = Cursor {
            chat_id: 42,
            topic_id: None,
            offset_id: 1,
            high_water: 10,
            done: true,
            pages: 1,
        };
        let messages = vec![Message {
            id: 1,
            date: 100,
            author: "Synthetic".into(),
            author_id: "user7".into(),
            text: "A synthetic memory".into(),
            reply_to: None,
            topic_id: None,
            edited_at: None,
            forwarded_from: None,
            forwarded_date: None,
            self_authored: true,
        }];
        Event::Page {
            page_id: page_digest(
                7,
                &Selection {
                    chat_id: 42,
                    topic_id: None,
                },
                &cursor,
                &messages,
            ),
            account_id: 7,
            chat_id: 42,
            title: "Synthetic chat".into(),
            cursor,
            messages,
        }
    }
    #[test]
    fn crash_safe_replay_and_source_materialization() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        j.commit_page(root.path(), &page()).unwrap();
        assert_eq!(j.pages.len(), 1);
        j.finish(root.path()).unwrap();
        assert!(j.pages.is_empty());
        let first = j.sources.clone();
        j.finish(root.path()).unwrap();
        assert_eq!(j.sources[0].file_id, first[0].file_id);
        let bytes = fs::read(root.path().join("uploads").join(&j.sources[0].storage_path)).unwrap();
        assert_eq!(digest(&bytes), j.sources[0].sha256);
        assert!(String::from_utf8(bytes).unwrap().contains("tg:7:42:1"));
    }
    #[test]
    fn refuses_scope_account_and_cursor_changes() {
        let (root, mut j) = setup();
        let mut e = page();
        if let Event::Page { account_id, .. } = &mut e {
            *account_id = 8;
        }
        assert!(j.commit_page(root.path(), &e).is_err());
        assert!(j.begin(j.scope.clone().unwrap(), 8).is_err());
        assert!(j.finish(root.path()).is_err());
        let mut scope = j.scope.clone().unwrap();
        scope.chats[0].chat_id = 99;
        assert!(j.begin(scope, 7).is_err());
    }
    #[test]
    fn corrupted_replay_is_not_accepted() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        let mut e = page();
        if let Event::Page { messages, .. } = &mut e {
            messages[0].text = "Different".into();
        }
        assert_eq!(j.commit_page(root.path(), &e), Err("invalid_page"));
    }
    #[test]
    fn journal_has_no_auth_material() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        let s = String::from_utf8(fs::read(j.folder(root.path()).join("journal.json")).unwrap())
            .unwrap();
        for secret in [
            "api_hash",
            "password",
            "phone_code",
            "auth_key",
            "bearer",
            "login_token",
        ] {
            assert!(!s.contains(secret));
        }
    }
    #[test]
    fn rejects_escaping_paths() {
        let (root, _) = setup();
        assert!(ensure_safe(root.path(), &root.path().join("../escape")).is_err());
        assert!(ensure_safe(root.path(), Path::new("C:/other")).is_err());
    }
    #[test]
    fn invalid_usage_is_rejected() {
        let (_, j) = setup();
        assert!(Journal::new(j.context, "other".into()).is_err());
    }
    #[test]
    fn metadata_accepts_progress_beyond_the_retired_global_message_cap() {
        let (_, mut j) = setup();
        j.pages = (0..1_200).map(|index| format!("{index:064x}")).collect();
        j.cursors = vec![Cursor {
            chat_id: 42,
            topic_id: None,
            offset_id: 1,
            high_water: 120_000,
            done: false,
            pages: 1_200,
        }];
        assert!(j.validate_metadata(&j.context, j.job_id).is_ok());
    }
    #[test]
    fn disk_corruption_is_rejected_before_materialization() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        let mut tampered = page();
        if let Event::Page { messages, .. } = &mut tampered {
            messages[0].text = "Tampered".into();
        }
        fs::write(
            j.folder(root.path()).join(format!("{}.json", j.pages[0])),
            serde_json::to_vec(&tampered).unwrap(),
        )
        .unwrap();
        assert_eq!(j.finish(root.path()), Err("checkpoint_invalid"));
        assert!(!j.complete);
        assert!(j.sources.is_empty());
    }
    #[test]
    fn resumes_same_journal_and_partitions_without_duplicate_source_ids() {
        let (root, mut j) = setup();
        for index in 0..8 {
            let start = 400 - index * 40;
            let cursor = Cursor {
                chat_id: 42,
                topic_id: None,
                offset_id: start - 39,
                high_water: 400,
                done: index == 7,
                pages: (index + 1) as usize,
            };
            let messages = (0..40)
                .map(|n| Message {
                    id: start - n,
                    text: "x".repeat(30000),
                    ..match page() {
                        Event::Page { messages, .. } => messages[0].clone(),
                        _ => unreachable!(),
                    }
                })
                .collect::<Vec<_>>();
            let e = Event::Page {
                page_id: page_digest(7, &j.scope.as_ref().unwrap().chats[0], &cursor, &messages),
                account_id: 7,
                chat_id: 42,
                title: "Synthetic partition test".into(),
                cursor,
                messages,
            };
            j.commit_page(root.path(), &e).unwrap();
        }
        let mut resumed: Journal =
            serde_json::from_slice(&fs::read(j.folder(root.path()).join("journal.json")).unwrap())
                .unwrap();
        resumed.finish(root.path()).unwrap();
        assert_eq!(resumed.sources.len(), 2);
        assert!(resumed.sources.iter().all(|s| s.size <= MAX_SOURCE));
        let ids = resumed
            .sources
            .iter()
            .map(|s| s.file_id)
            .collect::<Vec<_>>();
        resumed.finish(root.path()).unwrap();
        assert_eq!(
            ids,
            resumed
                .sources
                .iter()
                .map(|s| s.file_id)
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn partition_budget_counts_array_delimiters_exactly() {
        let (root, mut j) = setup();
        let template = match page() {
            Event::Page { messages, .. } => messages[0].clone(),
            _ => unreachable!(),
        };
        for index in 0..50 {
            let start = 5000 - index * 100;
            let cursor = Cursor {
                chat_id: 42,
                topic_id: None,
                offset_id: start - 99,
                high_water: 5000,
                done: index == 49,
                pages: (index + 1) as usize,
            };
            let messages = (0..100)
                .map(|offset| Message {
                    id: start - offset,
                    text: "x".repeat(1600),
                    ..template.clone()
                })
                .collect::<Vec<_>>();
            j.commit_page(
                root.path(),
                &Event::Page {
                    page_id: page_digest(
                        7,
                        &j.scope.as_ref().unwrap().chats[0],
                        &cursor,
                        &messages,
                    ),
                    account_id: 7,
                    chat_id: 42,
                    title: "Many short synthetic messages".into(),
                    cursor,
                    messages,
                },
            )
            .unwrap();
        }
        j.finish(root.path()).unwrap();
        assert!(j.sources.len() >= 2);
        assert!(j.sources.iter().all(|source| source.size <= MAX_SOURCE));
    }
    #[test]
    fn generated_source_uses_shared_parser_and_stable_citations() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        j.finish(root.path()).unwrap();
        let source = &j.sources[0];
        assert!(!source.original_filename.contains("42"));
        let bytes = fs::read(root.path().join("uploads").join(&source.storage_path)).unwrap();
        let result = synthius_dendrite_parser::parse(
            &source.original_filename,
            &bytes,
            "chat",
            "telegram_json",
            "Synthetic",
        )
        .unwrap();
        let jsonl = String::from_utf8(result.corpus).unwrap();
        assert!(jsonl.contains("tg:7:42:1"));
        assert!(jsonl.contains("A synthetic memory"));
        assert!(jsonl.contains("Synthetic"));
        assert_eq!(j.source_usage, "context_only");
    }
    #[test]
    fn cancellation_discards_checkpoints_and_prevents_resume() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        let path = j.folder(root.path()).join(format!("{}.json", j.pages[0]));
        j.discard(root.path()).unwrap();
        assert!(j.cancelled);
        assert!(!path.exists());
        assert!(j.pages.is_empty());
        assert!(j.begin(j.scope.clone().unwrap(), 7).is_err());
    }
    #[test]
    fn cancellation_preserves_completed_sources() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        j.finish(root.path()).unwrap();
        let path = root.path().join("uploads").join(&j.sources[0].storage_path);
        assert_eq!(j.discard(root.path()), Err("completed_source_preserved"));
        assert!(path.exists());
    }
    #[test]
    fn resume_preserves_original_consent_time() {
        let (_, mut j) = setup();
        let first = j.consented_at;
        j.begin(j.scope.clone().unwrap(), 7).unwrap();
        assert_eq!(j.consented_at, first);
    }
    #[test]
    fn cancellation_removes_only_a_durably_owned_partial_source() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        j.finish(root.path()).unwrap();
        let source = j.sources[0].clone();
        let path = root.path().join("uploads").join(&source.storage_path);
        // Recreate the exact durable state left by a crash after source write
        // and before the final publication save.
        j.complete = false;
        j.sources.clear();
        j.pending_sources.push(source);
        j.save(root.path()).unwrap();
        j.discard(root.path()).unwrap();
        assert!(!path.exists());
        assert!(j.pending_sources.is_empty());
        assert!(j.pages.is_empty());
    }
    #[test]
    fn cancellation_never_deletes_a_changed_local_file() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        j.finish(root.path()).unwrap();
        let source = j.sources[0].clone();
        let path = root.path().join("uploads").join(&source.storage_path);
        j.complete = false;
        j.sources.clear();
        j.pending_sources.push(source);
        fs::write(&path, b"different local content").unwrap();
        assert_eq!(j.discard(root.path()), Err("source_collision"));
        assert!(path.exists());
        assert!(!j.pending_sources.is_empty());
    }
    #[test]
    fn replay_rejects_a_cursor_bound_to_another_chat() {
        let (root, j) = setup();
        let mut event = page();
        let Event::Page {
            page_id,
            cursor,
            messages,
            ..
        } = &mut event
        else {
            unreachable!()
        };
        cursor.chat_id = 99;
        *page_id = page_digest(7, &j.scope.as_ref().unwrap().chats[0], cursor, messages);
        let id = page_id.clone();
        atomic_write(
            root.path(),
            &j.folder(root.path()).join(format!("{id}.json")),
            &serde_json::to_vec(&event).unwrap(),
            true,
        )
        .unwrap();
        assert!(matches!(
            j.read_page(root.path(), &id),
            Err("checkpoint_invalid")
        ));
    }
    #[test]
    fn journal_metadata_rejects_unknown_receipts_and_unsafe_sources() {
        let (root, mut j) = setup();
        j.commit_page(root.path(), &page()).unwrap();
        j.finish(root.path()).unwrap();
        assert!(j.validate_metadata(&j.context, j.job_id).is_ok());
        j.registered.push(Uuid::new_v4());
        assert_eq!(
            j.validate_metadata(&j.context, j.job_id),
            Err("journal_invalid")
        );
        j.registered.clear();
        j.sources[0].storage_path = "../foreign.json".into();
        assert_eq!(
            j.validate_metadata(&j.context, j.job_id),
            Err("journal_invalid")
        );
    }
}
