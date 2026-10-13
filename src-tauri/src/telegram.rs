//! Native-only authorization bridge. The loopback API can open a server-verified
//! intent and receive content-free source manifests; it cannot submit Telegram
//! login commands or obtain dialog names, QR codes, passwords or message pages.
use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Child, ChildStdin, Command as ProcessCommand, Stdio},
    sync::atomic::{AtomicBool, Ordering},
};
use synthius_dendrite_telegram::{
    journal::{ensure_safe, Journal},
    protocol::{self, Command, Context, Envelope, Event},
};
use zeroize::Zeroizing;

const WINDOW: &str = "telegram-import";
type OpenWindow = dyn Fn() -> Result<(), &'static str> + Send + Sync;

fn isolate_worker_environment(process: &mut ProcessCommand) {
    // The worker must not inherit bearer tokens, API keys, proxy credentials or
    // other application secrets. Windows nevertheless requires its runtime root
    // variables for native networking; removing them makes Telegram connection
    // setup fail before authorization. Preserve only those non-secret OS values.
    process.env_clear();
    #[cfg(windows)]
    for name in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(name) {
            process.env(name, value);
        }
    }
}

#[derive(Clone, Serialize, Default)]
pub struct NativeView {
    pub phase: String,
    pub error: Option<String>,
    pub event: Option<Event>,
    pub persona_name: String,
    pub source_usage: String,
    pub job_id: String,
    pub scope: Option<protocol::Scope>,
    pub pages: usize,
    pub total_pages: Option<usize>,
    pub chats_total: usize,
    pub chats_completed: usize,
    pub current_chat: usize,
    pub current_chat_pages: usize,
    pub sources: usize,
    pub logout_confirmed: Option<bool>,
    pub credentials_available: bool,
    pub prior_scopes: Vec<protocol::Scope>,
}
struct Active {
    journal: Journal,
    view: NativeView,
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
    nonce: String,
    finished: Arc<AtomicBool>,
    opened_at: Instant,
}
// These authenticated requests stay in Rust, never in webview state or journals.
// Each constructor uses the verified account and a fixed backend endpoint.
pub(super) struct ConsentAccess {
    user_id: Uuid,
    status: reqwest::RequestBuilder,
    acceptance: reqwest::RequestBuilder,
}
impl ConsentAccess {
    pub(super) fn new(
        user_id: Uuid,
        status: reqwest::RequestBuilder,
        acceptance: reqwest::RequestBuilder,
    ) -> Self {
        Self {
            user_id,
            status,
            acceptance,
        }
    }
    pub(super) async fn accepted(&self, accept: bool) -> Result<bool, &'static str> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Receipt {
            user_id: Uuid,
            accepted: bool,
            accepted_at: Option<chrono::DateTime<chrono::Utc>>,
        }
        let request = if accept {
            &self.acceptance
        } else {
            &self.status
        };
        let response = request
            .try_clone()
            .ok_or("consent_unavailable")?
            .send()
            .await
            .map_err(|_| "consent_unavailable")?;
        if !response.status().is_success() {
            return Err("consent_unavailable");
        }
        let receipt: Receipt = response.json().await.map_err(|_| "consent_unavailable")?;
        if receipt.user_id != self.user_id
            || receipt.accepted != receipt.accepted_at.is_some()
            || (accept && !receipt.accepted)
        {
            return Err("consent_unavailable");
        }
        Ok(receipt.accepted)
    }
}
struct PendingConsent {
    journal: Journal,
    access: Arc<ConsentAccess>,
    view: NativeView,
    accepting: bool,
}
pub struct Bridge {
    root: PathBuf,
    active: Mutex<Option<Active>>,
    pending_consent: Mutex<Option<PendingConsent>>,
    inactive_view: Mutex<Option<NativeView>>,
    open_lock: Mutex<()>,
    registration_lock: Mutex<()>,
    open_window: Mutex<Option<Box<OpenWindow>>>,
}
impl Bridge {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            active: Mutex::new(None),
            pending_consent: Mutex::new(None),
            inactive_view: Mutex::new(None),
            open_lock: Mutex::new(()),
            registration_lock: Mutex::new(()),
            open_window: Mutex::new(None),
        }
    }
    pub fn set_window(&self, callback: Box<OpenWindow>) {
        *self.open_window.lock().unwrap() = Some(callback);
    }
    fn load(&self, context: &Context, id: Uuid) -> Result<Journal, &'static str> {
        let path = self
            .root
            .join("metadata/telegram-imports")
            .join(context.user_id.to_string())
            .join(id.to_string())
            .join("journal.json");
        ensure_safe(&self.root, &path)?;
        if std::fs::metadata(&path)
            .map_err(|_| "import_unavailable")?
            .len()
            > 2 * 1024 * 1024
        {
            return Err("journal_invalid");
        }
        let j: Journal =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| "import_unavailable")?)
                .map_err(|_| "journal_invalid")?;
        if j.context.persona_id != context.persona_id {
            return Err("import_persona_mismatch");
        }
        j.validate_metadata(context, id)?;
        Ok(j)
    }
    pub(super) fn load_owned(
        &self,
        user_id: Uuid,
        device_id: Uuid,
        id: Uuid,
    ) -> Result<Journal, &'static str> {
        let path = self
            .root
            .join("metadata/telegram-imports")
            .join(user_id.to_string())
            .join(id.to_string())
            .join("journal.json");
        ensure_safe(&self.root, &path)?;
        if std::fs::metadata(&path)
            .map_err(|_| "import_unavailable")?
            .len()
            > 2 * 1024 * 1024
        {
            return Err("journal_invalid");
        }
        let journal: Journal =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| "import_unavailable")?)
                .map_err(|_| "journal_invalid")?;
        if journal.context.user_id != user_id || journal.context.device_id != device_id {
            return Err("import_identity_mismatch");
        }
        journal.validate_metadata(&journal.context, id)?;
        Ok(journal)
    }
    pub(super) fn mark_registered(
        &self,
        user_id: Uuid,
        device_id: Uuid,
        job_id: Uuid,
        source: &synthius_dendrite_telegram::journal::Source,
    ) -> Result<Journal, &'static str> {
        let _registration = self.registration_lock.lock().unwrap();
        let mut journal = self.load_owned(user_id, device_id, job_id)?;
        if !journal.complete
            || journal
                .sources
                .iter()
                .find(|candidate| candidate.file_id == source.file_id)
                != Some(source)
            || self.active.lock().unwrap().as_ref().is_some_and(|active| {
                active.journal.job_id == journal.job_id && active.view.phase != "complete"
            })
        {
            return Err("registration_not_ready");
        }
        if !journal.registered.contains(&source.file_id) {
            journal.registered.push(source.file_id);
            journal.save(&self.root)?;
        }
        if journal
            .sources
            .iter()
            .all(|candidate| journal.registered.contains(&candidate.file_id))
        {
            journal.clear_checkpoints(&self.root)?;
        }
        let mut active = self.active.lock().unwrap();
        if let Some(current) = active
            .as_mut()
            .filter(|active| active.journal.job_id == journal.job_id)
        {
            current.journal = journal.clone();
        }
        Ok(journal)
    }
    pub(super) fn open(
        self: &Arc<Self>,
        context: Context,
        usage: String,
        resume: Option<Uuid>,
        consent: ConsentAccess,
        accepted: bool,
    ) -> Result<Uuid, &'static str> {
        let _opening = self.open_lock.lock().unwrap();
        if context.user_id != consent.user_id {
            return Err("consent_unavailable");
        }
        if !synthius_dendrite_telegram::network::credentials_available() {
            return Err("app_credentials_missing");
        }
        {
            if self.pending_consent.lock().unwrap().is_some() {
                return Err("import_already_running");
            }
            let active = self.active.lock().unwrap();
            if let Some(a) = active.as_ref() {
                if !a.finished.load(Ordering::SeqCst) {
                    return Err("import_already_running");
                }
            }
        }
        let mut journal = if let Some(id) = resume {
            self.load(&context, id)?
        } else {
            Journal::new(context.clone(), usage.clone())?
        };
        if journal.complete || journal.cancelled {
            return Err("import_already_complete");
        }
        if journal.source_usage != usage {
            return Err("resume_identity_mismatch");
        }
        journal.context.persona_name = context.persona_name;
        *self.inactive_view.lock().unwrap() = None;
        let id = journal.job_id;
        if accepted {
            self.start_worker(journal)?;
        } else {
            self.queue_consent(journal, consent);
        }
        let window_result = self
            .open_window
            .lock()
            .unwrap()
            .as_ref()
            .ok_or("native_window_unavailable")
            .and_then(|open| open());
        if let Err(error) = window_result {
            self.shutdown_inner();
            return Err(error);
        }
        Ok(id)
    }
    fn queue_consent(&self, journal: Journal, access: ConsentAccess) {
        let view = NativeView {
            phase: "consent".into(),
            persona_name: journal.context.persona_name.clone(),
            source_usage: journal.source_usage.clone(),
            job_id: journal.job_id.to_string(),
            credentials_available: true,
            ..Default::default()
        };
        *self.pending_consent.lock().unwrap() = Some(PendingConsent {
            journal,
            access: Arc::new(access),
            view,
            accepting: false,
        });
    }
    fn begin_acceptance(&self, job_id: Uuid) -> Result<Arc<ConsentAccess>, &'static str> {
        let _opening = self.open_lock.lock().unwrap();
        let mut pending = self.pending_consent.lock().unwrap();
        let p = pending
            .as_mut()
            .filter(|p| p.journal.job_id == job_id)
            .ok_or("stale_import")?;
        if p.accepting {
            return Err("consent_in_progress");
        }
        p.accepting = true;
        p.view.error = None;
        Ok(p.access.clone())
    }
    fn acceptance_failed(&self, job_id: Uuid) {
        let mut pending = self.pending_consent.lock().unwrap();
        if let Some(p) = pending.as_mut().filter(|p| p.journal.job_id == job_id) {
            p.accepting = false;
            p.view.error = Some("consent_unavailable".into());
        }
    }
    // Called under open_lock after the durable receipt has been checked. A late
    // response cannot start a cancelled import or a different account's job.
    fn take_consented_journal(&self, job_id: Uuid) -> Result<Journal, &'static str> {
        let mut pending = self.pending_consent.lock().unwrap();
        if pending
            .as_ref()
            .is_none_or(|p| p.journal.job_id != job_id || !p.accepting)
        {
            return Err("stale_import");
        }
        Ok(pending.take().unwrap().journal)
    }
    async fn accept_consent(self: &Arc<Self>, job_id: Uuid) -> Result<(), &'static str> {
        let access = self.begin_acceptance(job_id)?;
        if let Err(error) = access.accepted(true).await {
            self.acceptance_failed(job_id);
            return Err(error);
        }
        let bridge = self.clone();
        tokio::task::spawn_blocking(move || {
            let _opening = bridge.open_lock.lock().unwrap();
            let journal = bridge.take_consented_journal(job_id)?;
            let mut view = NativeView {
                phase: "paused".into(),
                job_id: job_id.to_string(),
                persona_name: journal.context.persona_name.clone(),
                source_usage: journal.source_usage.clone(),
                ..Default::default()
            };
            if let Err(error) = bridge.start_worker(journal) {
                view.error = Some(error.into());
                *bridge.inactive_view.lock().unwrap() = Some(view);
                return Err(error);
            }
            Ok(())
        })
        .await
        .map_err(|_| "worker_unavailable")?
    }
    fn start_worker(self: &Arc<Self>, mut journal: Journal) -> Result<Uuid, &'static str> {
        // Persist non-session metadata before spawn. The revocation marker is
        // armed only after the worker and both private pipes exist.
        journal.save(&self.root)?;
        let mut process =
            ProcessCommand::new(std::env::current_exe().map_err(|_| "worker_unavailable")?);
        isolate_worker_environment(&mut process);
        process
            .arg("--telegram-import-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            process.creation_flags(0x08000000);
        }
        let mut child = process.spawn().map_err(|_| "worker_unavailable")?;
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("worker_unavailable");
        };
        let Some(mut stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err("worker_unavailable");
        };
        let prior_logout_unconfirmed = journal.logout_unconfirmed;
        journal.logout_unconfirmed = true;
        if let Err(error) = journal.save(&self.root) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let nonce = Uuid::new_v4().simple().to_string();
        if writeln!(stdin, "{nonce}")
            .and_then(|_| stdin.flush())
            .is_err()
        {
            journal.logout_unconfirmed = prior_logout_unconfirmed;
            let _ = journal.save(&self.root);
            let _ = child.kill();
            let _ = child.wait();
            return Err("worker_unavailable");
        }
        let finished = Arc::new(AtomicBool::new(false));
        let id = journal.job_id;
        let view = NativeView {
            phase: "connecting".into(),
            persona_name: journal.context.persona_name.clone(),
            source_usage: journal.source_usage.clone(),
            job_id: id.to_string(),
            scope: journal.scope.clone(),
            pages: journal.pages.len(),
            credentials_available: true,
            ..Default::default()
        };
        *self.active.lock().unwrap() = Some(Active {
            journal,
            view,
            stdin: Arc::new(Mutex::new(stdin)),
            child: Arc::new(Mutex::new(child)),
            nonce: nonce.clone(),
            finished: finished.clone(),
            opened_at: Instant::now(),
        });
        let bridge = self.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut failed = false;
            loop {
                let mut bytes = Zeroizing::new(Vec::new());
                if read_event_frame(&mut reader, &mut bytes).is_err() {
                    break;
                }
                let Ok(envelope) = serde_json::from_slice::<Envelope<Event>>(&bytes) else {
                    break;
                };
                if envelope.version != 1 || envelope.nonce != nonce {
                    break;
                }
                let terminal = matches!(envelope.payload, Event::LoggedOut { .. });
                if failed && !terminal {
                    continue;
                }
                if let Err(code) = bridge.event(id, envelope.payload) {
                    if let Some(a) = bridge
                        .active
                        .lock()
                        .unwrap()
                        .as_mut()
                        .filter(|a| a.journal.job_id == id)
                    {
                        a.view.error = Some(code.into());
                    }
                    let _ = bridge.send(Command::Cancel);
                    if terminal {
                        break;
                    }
                    failed = true;
                    continue;
                }
                if terminal {
                    break;
                }
            }
            let child = {
                let mut state = bridge.active.lock().unwrap();
                if let Some(a) = state.as_mut().filter(|a| a.journal.job_id == id) {
                    if a.view.logout_confirmed.is_none() {
                        a.view.logout_confirmed = Some(false);
                        a.view.phase = if a.journal.complete {
                            "complete"
                        } else {
                            "paused"
                        }
                        .into();
                        if a.view.error.is_none() {
                            a.view.error = Some("logout_unconfirmed".into());
                        }
                    }
                    Some(a.child.clone())
                } else {
                    None
                }
            };
            if let Some(child) = child {
                let deadline = Instant::now() + Duration::from_secs(8);
                loop {
                    if child.lock().unwrap().try_wait().ok().flatten().is_some() {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let mut child = child.lock().unwrap();
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
            }
            finished.store(true, Ordering::SeqCst);
        });
        Ok(id)
    }
    fn event(&self, id: Uuid, event: Event) -> Result<(), &'static str> {
        if matches!(&event, Event::Complete) {
            let mut journal = {
                let mut state = self.active.lock().unwrap();
                let a = state
                    .as_mut()
                    .filter(|a| a.journal.job_id == id)
                    .ok_or("import_unavailable")?;
                a.view.phase = "finishing".into();
                a.journal.clone()
            };
            // Native progress remains readable during potentially slow disk work.
            journal.finish(&self.root)?;
            let mut state = self.active.lock().unwrap();
            let a = state
                .as_mut()
                .filter(|a| a.journal.job_id == id)
                .ok_or("import_unavailable")?;
            a.view.sources = journal.sources.len();
            a.journal = journal;
            a.view.event = Some(event);
            return Ok(());
        }
        let ack;
        {
            let mut active = self.active.lock().unwrap();
            let a = active
                .as_mut()
                .filter(|a| a.journal.job_id == id)
                .ok_or("import_unavailable")?;
            ack = match &event {
                Event::Ready => {
                    a.view.phase = "login".into();
                    None
                }
                Event::Qr { .. } => {
                    a.view.phase = "qr".into();
                    None
                }
                Event::CodeRequired => {
                    a.view.phase = "code".into();
                    None
                }
                Event::PasswordRequired => {
                    a.view.phase = "password".into();
                    None
                }
                Event::Authorized { account_id, .. } => {
                    if a.journal.account_id.is_some_and(|old| old != *account_id) {
                        a.view.error = Some("resume_identity_mismatch".into());
                        return Err("resume_identity_mismatch");
                    }
                    a.journal.account_id = Some(*account_id);
                    a.journal.save(&self.root)?;
                    a.view.prior_scopes = self
                        .journals(
                            &a.journal.context.user_id.to_string(),
                            &a.journal.context.persona_id.to_string(),
                        )?
                        .into_iter()
                        .filter(|j| {
                            j.job_id != id && j.complete && j.account_id == Some(*account_id)
                        })
                        .filter_map(|j| j.scope)
                        .collect();
                    a.view.phase = "authorized".into();
                    None
                }
                Event::Catalog { .. } => {
                    a.view.phase = "select".into();
                    None
                }
                Event::Topics { .. } => {
                    a.view.phase = "select".into();
                    None
                }
                Event::Progress {
                    total_chats,
                    completed_chats,
                    current_chat,
                    current_pages,
                    total_pages,
                } => {
                    a.view.phase = "exporting".into();
                    a.view.chats_total = *total_chats;
                    a.view.chats_completed = *completed_chats;
                    a.view.current_chat = *current_chat;
                    a.view.current_chat_pages = *current_pages;
                    a.view.total_pages = *total_pages;
                    None
                }
                Event::Page {
                    page_id, cursor, ..
                } => {
                    a.journal.commit_page(&self.root, &event)?;
                    a.view.phase = "exporting".into();
                    a.view.pages = a.journal.pages.len();
                    a.view.current_chat_pages = cursor.pages;
                    a.view.chats_completed = a.journal.cursors.iter().filter(|c| c.done).count();
                    Some(page_id.clone())
                }
                Event::Complete => unreachable!("handled outside the state lock"),
                Event::Error { code } => {
                    a.view.error = Some(code.clone());
                    None
                }
                Event::LoggedOut { confirmed } => {
                    a.journal.logout_unconfirmed = !*confirmed;
                    if a.journal.complete && a.journal.sources.is_empty() {
                        a.journal.clear_checkpoints(&self.root)?;
                    }
                    a.journal.save(&self.root)?;
                    a.view.logout_confirmed = Some(*confirmed);
                    a.view.phase = if a.journal.complete {
                        "complete"
                    } else {
                        "paused"
                    }
                    .into();
                    if !confirmed && a.view.error.is_none() {
                        a.view.error = Some("logout_unconfirmed".into());
                    }
                    None
                }
            };
            // Only native UI sees catalog/QR; raw pages are never retained in UI state.
            if !matches!(&event, Event::Page { .. }) {
                a.view.event = Some(event);
            }
        }
        if let Some(page_id) = ack {
            self.send(Command::Ack { page_id })?;
        }
        Ok(())
    }
    pub fn view(&self) -> NativeView {
        if let Some(pending) = self.pending_consent.lock().unwrap().as_ref() {
            return pending.view.clone();
        }
        if let Some(view) = self.inactive_view.lock().unwrap().as_ref() {
            return view.clone();
        }
        self.active
            .lock()
            .unwrap()
            .as_ref()
            .map(|a| a.view.clone())
            .unwrap_or(NativeView {
                phase: "idle".into(),
                credentials_available: synthius_dendrite_telegram::network::credentials_available(),
                ..Default::default()
            })
    }
    fn send_for_job(&self, job_id: Uuid, command: Command) -> Result<(), &'static str> {
        let _opening = self.open_lock.lock().unwrap();
        if self.pending_consent.lock().unwrap().is_some() {
            return Err("consent_required");
        }
        if self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|a| a.journal.job_id != job_id)
        {
            return Err("stale_import");
        }
        self.send(command)
    }
    pub fn send(&self, command: Command) -> Result<(), &'static str> {
        let (stdin, nonce) = {
            let mut active = self.active.lock().unwrap();
            let a = active.as_mut().ok_or("import_unavailable")?;
            if a.finished.load(Ordering::SeqCst) {
                return Err("import_unavailable");
            }
            if !command_allowed(&a.view.phase, &command) {
                return Err("invalid_state");
            }
            if let Command::Export {
                scope,
                cursors: _,
                account_id: _,
            } = &command
            {
                let account = a.journal.account_id;
                // Authorized account is supplied from the worker, never the web.
                let account = account.ok_or("login_required")?;
                a.journal.begin(scope.clone(), account)?;
                a.journal.save(&self.root)?;
                a.view.scope = Some(scope.clone());
                a.view.phase = "exporting".into();
                a.view.total_pages = None;
                a.view.chats_total = scope.chats.len();
                a.view.chats_completed = a.journal.cursors.iter().filter(|c| c.done).count();
                a.view.current_chat = 0;
                a.view.current_chat_pages = 0;
            }
            if matches!(&command, Command::Qr | Command::Phone { .. })
                && a.opened_at.elapsed() > Duration::from_secs(10 * 60)
            {
                return Err("intent_expired");
            }
            if !matches!(&command, Command::Ack { .. }) {
                a.view.error = None;
            }
            (a.stdin.clone(), a.nonce.clone())
        };
        let mut bytes = Zeroizing::new(
            serde_json::to_vec(&Envelope {
                version: 1,
                nonce,
                payload: command,
            })
            .map_err(|_| "invalid_command")?,
        );
        if bytes.len() > protocol::MAX_FRAME {
            return Err("invalid_command");
        }
        bytes.push(b'\n');
        let mut writer = stdin.lock().unwrap();
        writer
            .write_all(&bytes)
            .and_then(|_| writer.flush())
            .map_err(|_| "worker_unavailable")
    }
    pub fn export_command(&self, scope: protocol::Scope) -> Result<Command, &'static str> {
        let state = self.active.lock().unwrap();
        let a = state.as_ref().ok_or("import_unavailable")?;
        Ok(Command::Export {
            scope,
            cursors: a.journal.cursors.clone(),
            account_id: a.journal.account_id,
        })
    }
    pub fn shutdown(&self) {
        let _opening = self.open_lock.lock().unwrap();
        self.shutdown_inner();
    }
    fn shutdown_inner(&self) {
        if let Some(pending) = self.pending_consent.lock().unwrap().take() {
            *self.inactive_view.lock().unwrap() = Some(NativeView {
                phase: "declined".into(),
                ..pending.view
            });
        }
        let _ = self.send(Command::Cancel);
        let (finished, child) = {
            let a = self.active.lock().unwrap();
            let Some(a) = a.as_ref() else {
                return;
            };
            (a.finished.clone(), a.child.clone())
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        while !finished.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(40));
        }
        if !finished.load(Ordering::SeqCst) {
            let _ = child.lock().unwrap().kill();
        }
    }
    fn discard(&self, job_id: Uuid) -> Result<(), &'static str> {
        let _opening = self.open_lock.lock().unwrap();
        {
            let mut pending = self.pending_consent.lock().unwrap();
            if pending.as_ref().is_some_and(|p| p.journal.job_id != job_id) {
                return Err("stale_import");
            }
            if pending.as_ref().is_some_and(|p| p.journal.job_id == job_id) {
                let p = pending.take().unwrap();
                *self.inactive_view.lock().unwrap() = Some(NativeView {
                    phase: "declined".into(),
                    ..p.view
                });
                return Ok(());
            }
        }
        if self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(|a| a.journal.job_id != job_id)
        {
            return Err("stale_import");
        }
        self.shutdown_inner();
        let finished = self
            .active
            .lock()
            .unwrap()
            .as_ref()
            .map(|a| a.finished.clone())
            .ok_or("import_unavailable")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !finished.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(40));
        }
        let mut active = self.active.lock().unwrap();
        let a = active.as_mut().ok_or("import_unavailable")?;
        if !a.finished.load(Ordering::SeqCst) {
            return Err("cleanup_in_progress");
        }
        a.journal.discard(&self.root)?;
        a.view.phase = "cancelled".into();
        a.view.pages = 0;
        Ok(())
    }
    pub fn journals(&self, user: &str, persona: &str) -> Result<Vec<Journal>, &'static str> {
        self.journal_catalogue(user, persona)
            .map(|(journals, _)| journals)
    }
    pub fn journal_catalogue(
        &self,
        user: &str,
        persona: &str,
    ) -> Result<(Vec<Journal>, usize), &'static str> {
        self.journal_catalogue_for_context(&Context {
            user_id: Uuid::parse_str(user).map_err(|_| "import_unavailable")?,
            persona_id: Uuid::parse_str(persona).map_err(|_| "import_unavailable")?,
            device_id: Uuid::parse_str(
                &load_or_create_device_id().map_err(|_| "device_unavailable")?,
            )
            .map_err(|_| "device_unavailable")?,
            persona_name: String::new(),
        })
    }
    fn journal_catalogue_for_context(
        &self,
        context: &Context,
    ) -> Result<(Vec<Journal>, usize), &'static str> {
        let folder = self
            .root
            .join("metadata/telegram-imports")
            .join(context.user_id.to_string());
        ensure_safe(&self.root, &folder)?;
        if !folder.exists() {
            return Ok((vec![], 0));
        }
        let mut out = vec![];
        let mut invalid = 0;
        // Do not truncate arbitrary directory order: that can silently hide a
        // resumable import or an unpublished completed source from its owner.
        for entry in std::fs::read_dir(folder).map_err(|_| "storage_unavailable")? {
            let Ok(entry) = entry else {
                continue;
            };
            let Ok(id) = Uuid::parse_str(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            match self.load(context, id) {
                Ok(mut j) => {
                    // Retrying a cancelled job's cleanup is safe: cancellation waits
                    // for its worker to exit, and cancelled plans cannot be reopened.
                    if j.cancelled && (!j.pages.is_empty() || !j.pending_sources.is_empty()) {
                        if j.discard(&self.root).is_err() {
                            invalid += 1;
                        }
                    } else if j.complete && !j.pages.is_empty() {
                        // A crash or file lock after durable source publication must
                        // not leave redundant plaintext checkpoints indefinitely.
                        let _ = j.clear_checkpoints(&self.root);
                    }
                    out.push(j);
                }
                Err("import_persona_mismatch") => {}
                Err(_) if entry.path().is_dir() || entry.path().is_symlink() => {
                    // Never silently hide a damaged record that may still own
                    // plaintext checkpoints. The web UI gives the owner a safe
                    // recovery warning without exposing its path or contents.
                    invalid += 1;
                }
                Err(_) => {}
            }
        }
        out.sort_by_key(|j| (std::cmp::Reverse(j.consented_at), j.job_id));
        Ok((out, invalid))
    }
}
fn read_event_frame(input: &mut impl BufRead, bytes: &mut Vec<u8>) -> Result<(), &'static str> {
    loop {
        let buffer = input.fill_buf().map_err(|_| "worker_unavailable")?;
        if buffer.is_empty() {
            return Err("worker_unavailable");
        }
        let count = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buffer.len(), |p| p + 1);
        if bytes.len() + count > protocol::MAX_FRAME {
            return Err("invalid_frame");
        }
        let ended = buffer[count - 1] == b'\n';
        bytes.extend_from_slice(&buffer[..count]);
        input.consume(count);
        if ended {
            return Ok(());
        }
    }
}
fn guard(window: &tauri::WebviewWindow) -> Result<(), String> {
    let url = window.url().map_err(|_| "Native import window required")?;
    if native_window_allowed(window.label(), &url) {
        Ok(())
    } else {
        Err("Native import window required".into())
    }
}
fn native_window_allowed(label: &str, url: &tauri::Url) -> bool {
    label == WINDOW
        && url.path() == "/telegram.html"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost")))
}
fn command_allowed(phase: &str, command: &Command) -> bool {
    match command {
        Command::Cancel => true,
        Command::Ack { .. } => phase == "exporting",
        Command::Qr | Command::PollQr | Command::Phone { .. } => matches!(phase, "login" | "qr"),
        Command::Code { .. } => phase == "code",
        Command::Password { .. } => phase == "password",
        Command::Catalog => matches!(phase, "authorized" | "select"),
        Command::Topics { .. } | Command::Export { .. } => phase == "select",
    }
}
#[tauri::command]
pub fn telegram_native_state(
    window: tauri::WebviewWindow,
    bridge: tauri::State<Arc<Bridge>>,
) -> Result<NativeView, String> {
    guard(&window)?;
    Ok(bridge.view())
}
#[tauri::command]
pub fn telegram_native_command(
    window: tauri::WebviewWindow,
    bridge: tauri::State<Arc<Bridge>>,
    job_id: Uuid,
    command: serde_json::Value,
) -> Result<(), String> {
    guard(&window)?;
    let raw = Zeroizing::new(serde_json::to_vec(&command).map_err(|_| "Invalid command")?);
    if raw.len() > 16 * 1024 {
        return Err("Invalid command".into());
    }
    let parsed = protocol::parse_command(command).map_err(|_| "Invalid command")?;
    let command = match &parsed {
        Command::Ack { .. } => return Err("Invalid command".into()),
        Command::Export { scope, .. } => bridge
            .export_command(scope.clone())
            .map_err(str::to_string)?,
        _ => parsed,
    };
    bridge.send_for_job(job_id, command).map_err(str::to_string)
}
#[tauri::command]
pub async fn telegram_native_accept_consent(
    window: tauri::WebviewWindow,
    bridge: tauri::State<'_, Arc<Bridge>>,
    job_id: Uuid,
) -> Result<(), String> {
    guard(&window)?;
    bridge
        .inner()
        .clone()
        .accept_consent(job_id)
        .await
        .map_err(str::to_string)
}
#[tauri::command]
pub async fn telegram_native_discard(
    window: tauri::WebviewWindow,
    bridge: tauri::State<'_, Arc<Bridge>>,
    job_id: Uuid,
) -> Result<(), String> {
    guard(&window)?;
    let bridge = bridge.inner().clone();
    tokio::task::spawn_blocking(move || bridge.discard(job_id))
        .await
        .map_err(|_| "Cleanup unavailable".to_string())?
        .map_err(str::to_string)
}
#[tauri::command]
pub async fn telegram_native_register(
    window: tauri::WebviewWindow,
    state: tauri::State<'_, Arc<Mutex<AppState>>>,
    job_id: Uuid,
) -> Result<serde_json::Value, String> {
    guard(&window)?;
    let (port, page_token) = {
        let state = state.lock().map_err(|_| "Local service unavailable")?;
        if state.server_state != ServerState::Running {
            return Err("Local service unavailable".into());
        }
        (state.port, state.update_runtime.page_token().to_string())
    };
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(90))
        .build()
        .map_err(|_| "Local source registration unavailable")?
        .post(format!(
            "http://127.0.0.1:{port}/api/desktop/telegram/register"
        ))
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .header("X-Dendrite-Update-Token", page_token)
        .json(&json!({"job_id":job_id}))
        .send()
        .await
        .map_err(|_| "Local source registration unavailable")?;
    if !response.status().is_success() {
        return Err("Local source registration did not finish".into());
    }
    response
        .json()
        .await
        .map_err(|_| "Local source registration response was incomplete".into())
}
#[tauri::command]
pub fn telegram_native_open_sources(
    window: tauri::WebviewWindow,
    persona_id: Uuid,
) -> Result<(), String> {
    guard(&window)?;
    let url = format!("{}?persona={persona_id}", configured_web_app_url());
    open::that(url).map_err(|_| "Synthius could not be opened".into())
}
pub fn show_window(app: &tauri::AppHandle) -> Result<(), &'static str> {
    if let Some(window) = app.get_webview_window(WINDOW) {
        window
            .eval("restartImport()")
            .map_err(|_| "native_window_unavailable")?;
        window.show().map_err(|_| "native_window_unavailable")?;
        let _ = window.set_focus();
        return Ok(());
    }
    WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("telegram.html".into()))
        .title(format!("Telegram snapshot · {}", super::PRODUCT_NAME))
        .inner_size(700., 760.)
        .min_inner_size(560., 600.)
        .on_navigation(|url| native_window_allowed(WINDOW, url))
        .build()
        .map(|_| ())
        .map_err(|_| "native_window_unavailable")
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_worker_environment_is_secret_free() {
        let mut process = ProcessCommand::new("synthetic-worker");
        isolate_worker_environment(&mut process);
        let configured = process
            .get_envs()
            .filter_map(|(name, value)| value.map(|_| name.to_string_lossy().to_string()))
            .collect::<Vec<_>>();
        #[cfg(windows)]
        {
            assert!(configured
                .iter()
                .all(|name| matches!(name.as_str(), "SystemRoot" | "WINDIR")));
            for name in ["SystemRoot", "WINDIR"] {
                if std::env::var_os(name).is_some() {
                    assert!(configured.iter().any(|configured| configured == name));
                }
            }
        }
        #[cfg(not(windows))]
        assert!(configured.is_empty());
    }

    fn pending_bridge() -> (tempfile::TempDir, Arc<Bridge>, Uuid) {
        let root = tempfile::tempdir().unwrap();
        let bridge = Arc::new(Bridge::new(root.path().to_path_buf()));
        let context = Context {
            user_id: Uuid::new_v4(),
            persona_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            persona_name: "Synthetic consent account".into(),
        };
        let journal = Journal::new(context.clone(), "context_only".into()).unwrap();
        let id = journal.job_id;
        let client = reqwest::Client::new();
        let access = ConsentAccess::new(
            context.user_id,
            client.get("http://127.0.0.1:1/consent"),
            client
                .post("http://127.0.0.1:1/consent")
                .json(&json!({"accepted":true})),
        );
        bridge.queue_consent(journal, access);
        (root, bridge, id)
    }
    #[test]
    fn first_use_starts_no_worker_and_requires_the_exact_pending_intent() {
        let (root, bridge, id) = pending_bridge();
        assert_eq!(bridge.view().phase, "consent");
        assert!(bridge.active.lock().unwrap().is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        assert_eq!(
            bridge.send_for_job(id, Command::Qr),
            Err("consent_required")
        );
        assert_eq!(
            bridge.begin_acceptance(Uuid::new_v4()).err(),
            Some("stale_import")
        );
        bridge.begin_acceptance(id).unwrap();
        assert_eq!(
            bridge.begin_acceptance(id).err(),
            Some("consent_in_progress")
        );
        bridge.acceptance_failed(id);
        assert_eq!(bridge.view().phase, "consent");
        bridge.begin_acceptance(id).unwrap();
        let journal = bridge.take_consented_journal(id).unwrap();
        assert_eq!(journal.job_id, id);
        assert!(bridge.pending_consent.lock().unwrap().is_none());
    }
    #[test]
    fn declining_does_not_write_a_receipt_or_modify_a_resumable_journal() {
        let (root, bridge, id) = pending_bridge();
        // A resumed journal already exists. Declining must leave it untouched.
        bridge
            .pending_consent
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .journal
            .save(root.path())
            .unwrap();
        let before = std::fs::read(
            root.path()
                .join("metadata/telegram-imports")
                .join(
                    bridge
                        .pending_consent
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .journal
                        .context
                        .user_id
                        .to_string(),
                )
                .join(id.to_string())
                .join("journal.json"),
        )
        .unwrap();
        bridge.begin_acceptance(id).unwrap();
        assert_eq!(bridge.discard(Uuid::new_v4()), Err("stale_import"));
        bridge.discard(id).unwrap();
        assert_eq!(bridge.view().phase, "declined");
        assert_eq!(
            bridge.take_consented_journal(id).err(),
            Some("stale_import")
        );
        let path = root.path().join("metadata/telegram-imports");
        let owner = std::fs::read_dir(path)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            std::fs::read(owner.join(id.to_string()).join("journal.json")).unwrap(),
            before
        );
        assert!(bridge.active.lock().unwrap().is_none());
    }
    #[tokio::test]
    async fn failed_consent_save_remains_pending_and_can_be_retried() {
        let (root, bridge, id) = pending_bridge();
        assert_eq!(bridge.accept_consent(id).await, Err("consent_unavailable"));
        assert_eq!(bridge.view().phase, "consent");
        assert!(
            !bridge
                .pending_consent
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .accepting
        );
        assert!(bridge.active.lock().unwrap().is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[tokio::test]
    async fn receipts_are_account_bound_and_acceptance_must_be_durable() {
        use axum::{routing::get, Router};
        let user = Uuid::new_v4();
        for (owner, accepted, timestamp, valid) in [
            (user, false, None, true),
            (user, true, Some("2026-10-12T12:00:00Z"), true),
            (Uuid::new_v4(), true, Some("2026-10-12T12:00:00Z"), false),
            (user, true, None, false),
            (user, false, Some("2026-10-12T12:00:00Z"), false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/consent", listener.local_addr().unwrap());
            let app = Router::new().route(
                "/consent",
                get(move || async move {
                    Json(json!({"user_id":owner,"accepted":accepted,"accepted_at":timestamp}))
                })
                .post(
                    move |headers: HeaderMap, Json(body): Json<serde_json::Value>| async move {
                        assert_eq!(headers[AUTHORIZATION], "Bearer synthetic-consent-token");
                        assert_eq!(body, json!({"accepted":true}));
                        Json(json!({"user_id":owner,"accepted":accepted,"accepted_at":timestamp}))
                    },
                ),
            );
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = reqwest::Client::new();
            let access = ConsentAccess::new(
                user,
                client.get(&url),
                client
                    .post(&url)
                    .header(AUTHORIZATION, "Bearer synthetic-consent-token")
                    .json(&json!({"accepted":true})),
            );
            assert_eq!(access.accepted(false).await.is_ok(), valid);
            assert_eq!(access.accepted(true).await.is_ok(), valid && accepted);
            server.abort();
        }
    }
    #[tokio::test]
    async fn late_saved_receipt_cannot_start_a_cancelled_or_replaced_job() {
        use axum::{routing::post, Router};
        let (root, bridge, id) = pending_bridge();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/consent", listener.local_addr().unwrap());
        let user = bridge
            .pending_consent
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .journal
            .context
            .user_id;
        let notify = entered.clone();
        let wait = release.clone();
        let app = Router::new().route("/consent", post(move || {
            let notify = notify.clone(); let wait = wait.clone();
            async move { notify.notify_one(); wait.notified().await;
                Json(json!({"user_id":user,"accepted":true,"accepted_at":"2026-10-12T12:00:00Z"})) }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::new();
        bridge
            .pending_consent
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .access = Arc::new(ConsentAccess::new(
            user,
            client.get(&url),
            client.post(&url).json(&json!({"accepted":true})),
        ));
        let accepting = bridge.clone();
        let task = tokio::spawn(async move { accepting.accept_consent(id).await });
        tokio::time::timeout(Duration::from_secs(3), entered.notified())
            .await
            .unwrap();
        bridge.discard(id).unwrap();
        release.notify_one();
        assert_eq!(task.await.unwrap(), Err("stale_import"));
        assert!(bridge.active.lock().unwrap().is_none());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        server.abort();
    }
    #[test]
    fn journal_catalogue_preserves_old_jobs_and_exact_identity() {
        let root = tempfile::tempdir().unwrap();
        let bridge = Bridge::new(root.path().to_path_buf());
        let context = Context {
            user_id: Uuid::new_v4(),
            persona_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            persona_name: "Synthetic owner".into(),
        };
        for _ in 0..1001 {
            Journal::new(context.clone(), "context_only".into())
                .unwrap()
                .save(root.path())
                .unwrap();
        }
        for different in [
            Context {
                persona_id: Uuid::new_v4(),
                ..context.clone()
            },
            Context {
                device_id: Uuid::new_v4(),
                ..context.clone()
            },
            Context {
                user_id: Uuid::new_v4(),
                ..context.clone()
            },
        ] {
            Journal::new(different, "context_only".into())
                .unwrap()
                .save(root.path())
                .unwrap();
        }
        let (rows, warnings) = bridge.journal_catalogue_for_context(&context).unwrap();
        assert_eq!(rows.len(), 1001);
        assert_eq!(
            warnings, 1,
            "a stale foreign-device journal is visible as a recovery warning"
        );
        assert!(rows.iter().all(|j| {
            j.context.user_id == context.user_id
                && j.context.persona_id == context.persona_id
                && j.context.device_id == context.device_id
        }));
    }
    #[test]
    fn journal_catalogue_retries_cleanup_after_registration_crash() {
        let root = tempfile::tempdir().unwrap();
        let bridge = Bridge::new(root.path().to_path_buf());
        let context = Context {
            user_id: Uuid::new_v4(),
            persona_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            persona_name: "Synthetic owner".into(),
        };
        let selection = protocol::Selection {
            chat_id: 42,
            topic_id: None,
        };
        let mut journal = Journal::new(context.clone(), "context_only".into()).unwrap();
        journal
            .begin(
                protocol::Scope {
                    chats: vec![selection.clone()],
                    from: 1,
                    until: 200,
                },
                7,
            )
            .unwrap();
        let cursor = protocol::Cursor {
            chat_id: 42,
            topic_id: None,
            offset_id: 1,
            high_water: 1,
            done: true,
            pages: 1,
        };
        let messages = vec![protocol::Message {
            id: 1,
            date: 100,
            author_id: "user7".into(),
            author: "Synthetic".into(),
            text: "Synthetic memory".into(),
            reply_to: None,
            topic_id: None,
            edited_at: None,
            forwarded_from: None,
            forwarded_date: None,
            self_authored: true,
        }];
        let page_id =
            synthius_dendrite_telegram::journal::page_digest(7, &selection, &cursor, &messages);
        journal
            .commit_page(
                root.path(),
                &Event::Page {
                    page_id,
                    account_id: 7,
                    chat_id: 42,
                    title: "Synthetic chat".into(),
                    cursor,
                    messages,
                },
            )
            .unwrap();
        let retained_page = journal.pages[0].clone();
        let checkpoint_path = journal
            .folder(root.path())
            .join(format!("{retained_page}.json"));
        let checkpoint_bytes = std::fs::read(&checkpoint_path).unwrap();
        journal.finish(root.path()).unwrap();
        let source_path = root
            .path()
            .join("uploads")
            .join(&journal.sources[0].storage_path);
        journal.registered.push(journal.sources[0].file_id);
        // Recreate the exact final-journal-before-cleanup crash state.
        std::fs::write(&checkpoint_path, checkpoint_bytes).unwrap();
        journal.pages.push(retained_page);
        journal.save(root.path()).unwrap();
        assert!(!journal.pages.is_empty());

        let rows = bridge.journal_catalogue_for_context(&context).unwrap().0;
        assert_eq!(rows.len(), 1);
        assert!(rows[0].pages.is_empty());
        assert!(source_path.exists());
    }
    #[test]
    fn journal_catalogue_surfaces_damaged_records_without_exposing_them() {
        let root = tempfile::tempdir().unwrap();
        let bridge = Bridge::new(root.path().to_path_buf());
        let context = Context {
            user_id: Uuid::new_v4(),
            persona_id: Uuid::new_v4(),
            device_id: Uuid::new_v4(),
            persona_name: "Synthetic owner".into(),
        };
        let journal = Journal::new(context.clone(), "context_only".into()).unwrap();
        journal.save(root.path()).unwrap();
        std::fs::write(
            journal.folder(root.path()).join("journal.json"),
            b"damaged synthetic journal",
        )
        .unwrap();
        let (rows, warnings) = bridge.journal_catalogue_for_context(&context).unwrap();
        assert!(rows.is_empty());
        assert_eq!(warnings, 1);
    }
    #[test]
    fn native_commands_for_a_stale_job_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let bridge = Bridge::new(root.path().to_path_buf());
        assert_eq!(
            bridge.send_for_job(Uuid::new_v4(), Command::Qr),
            Err("stale_import")
        );
        assert_eq!(bridge.discard(Uuid::new_v4()), Err("stale_import"));
    }
    #[test]
    fn native_commands_reject_web_and_other_windows() {
        assert!(native_window_allowed(
            WINDOW,
            &"http://tauri.localhost/telegram.html".parse().unwrap()
        ));
        for (label, url) in [
            ("dendrite", "http://tauri.localhost/telegram.html"),
            (WINDOW, "http://localhost:3000/telegram.html"),
            (WINDOW, "http://127.0.0.1:1543/telegram.html"),
            (WINDOW, "http://tauri.localhost/telegram.html?key=x"),
        ] {
            assert!(!native_window_allowed(label, &url.parse().unwrap()));
        }
    }
    #[test]
    fn duplicate_exports_and_commands_during_export_are_rejected() {
        let export = Command::Export {
            scope: protocol::Scope {
                chats: vec![],
                from: 1,
                until: 2,
            },
            cursors: vec![],
            account_id: None,
        };
        assert!(command_allowed("select", &export));
        assert!(!command_allowed("exporting", &export));
        assert!(!command_allowed("exporting", &Command::Catalog));
        assert!(command_allowed("exporting", &Command::Cancel));
        assert!(!command_allowed("paused", &Command::Qr));
    }
    #[test]
    fn bounded_child_frames() {
        let bytes = vec![b'x'; protocol::MAX_FRAME + 1];
        let mut input = BufReader::new(bytes.as_slice());
        assert!(read_event_frame(&mut input, &mut vec![]).is_err());
    }
}
