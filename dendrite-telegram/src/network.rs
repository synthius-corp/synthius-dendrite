//! All permitted account operations are visible here. No send/delete/join RPCs,
//! persistent Session storage or provider/cloud credentials are available.
use crate::{journal::digest, protocol::*};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::FutureExt;
use grammers_client::{
    client::{ClientConfiguration, PasswordToken},
    Client,
};
use grammers_mtsender::{InvocationError, SenderPool};
use grammers_session::{
    storages::MemorySession,
    types::{PeerId, PeerRef},
    Session,
};
use grammers_tl_types as tl;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, watch},
};
use zeroize::Zeroizing;

pub fn credentials_available() -> bool {
    credentials().is_some()
}
fn credentials() -> Option<(i32, &'static str)> {
    parse_credentials(
        option_env!("SYNTHIUS_TELEGRAM_API_ID")?,
        option_env!("SYNTHIUS_TELEGRAM_API_HASH")?,
    )
}
fn parse_credentials<'a>(id: &str, hash: &'a str) -> Option<(i32, &'a str)> {
    let id = id.parse::<i32>().ok()?;
    (id > 0 && hash.len() == 32 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .then_some((id, hash))
}
pub fn run_worker() {
    // A panic must never print account data. Catching SDK panics still runs logout.
    std::panic::set_hook(Box::new(|_| {}));
    if let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        runtime.block_on(worker());
    }
}
async fn emit(nonce: &str, event: Event) -> Result<(), &'static str> {
    let mut bytes = Zeroizing::new(
        serde_json::to_vec(&Envelope {
            version: crate::PROTOCOL_VERSION,
            nonce: nonce.into(),
            payload: event,
        })
        .map_err(|_| "protocol_error")?,
    );
    if bytes.len() > MAX_FRAME {
        return Err("invalid_frame");
    }
    bytes.push(b'\n');
    let mut out = tokio::io::stdout();
    out.write_all(&bytes)
        .await
        .map_err(|_| "host_disconnected")?;
    out.flush().await.map_err(|_| "host_disconnected")
}
async fn worker() {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut first = Zeroizing::new(Vec::new());
    if !matches!(
        tokio::time::timeout(Duration::from_secs(10), read_frame(&mut input, &mut first)).await,
        Ok(Ok(()))
    ) || first.len() > 128
    {
        return;
    }
    let Ok(first) = std::str::from_utf8(&first) else {
        return;
    };
    let nonce = first.trim().to_string();
    if nonce.len() != 32 || !nonce.chars().all(|c| c.is_ascii_hexdigit()) {
        return;
    }
    let Some((api_id, api_hash)) = credentials() else {
        let _ = emit(
            &nonce,
            Event::Error {
                code: "app_credentials_missing".into(),
            },
        )
        .await;
        return;
    };
    let session = Arc::new(MemorySession::default());
    let pool = SenderPool::with_configuration(
        session.clone(),
        api_id,
        grammers_mtsender::ConnectionParams {
            device_model: "Synthius Dendrite · one-time snapshot".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        },
    );
    let client = Client::with_configuration(
        pool.handle,
        ClientConfiguration {
            retry_policy: Box::new(grammers_client::client::AutoSleep {
                threshold: Duration::from_secs(30),
                io_errors_as_flood_of: Some(Duration::from_secs(1)),
            }),
            auto_cache_peers: false,
        },
    );
    let runner = tokio::spawn(pool.runner.run());
    // Discard live updates immediately: this is a snapshot, not a message listener.
    let login_update = Arc::new(AtomicBool::new(false));
    let update_flag = login_update.clone();
    let mut updates = pool.updates;
    let update_task = tokio::spawn(async move {
        while let Some(update) = updates.recv().await {
            if has_login_update(update) {
                update_flag.store(true, Ordering::SeqCst);
            }
        }
    });
    let (tx, mut commands) = mpsc::channel(8);
    let (cancel_tx, mut cancel) = watch::channel(false);
    let reader_nonce = nonce.clone();
    let reader = tokio::spawn(async move {
        loop {
            let mut bytes = Zeroizing::new(Vec::new());
            // fill_buf/consume bounds allocation even for a malicious unterminated frame.
            let result = read_frame(&mut input, &mut bytes).await;
            if result.is_err() {
                break;
            }
            match decode_command(&bytes, &reader_nonce) {
                Ok(Command::Cancel) => break,
                Ok(cmd) => match tx.try_send(cmd) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(Command::PollQr)) => {}
                    Err(_) => break,
                },
                Err(_) => break,
            }
        }
        let _ = cancel_tx.send(true);
    });
    let mut connection = Connection {
        client: client.clone(),
        session,
        api_id,
        api_hash,
        nonce: nonce.clone(),
        phone: None,
        code_hash: None,
        account: None,
        dialogs: HashMap::new(),
        topics: HashMap::new(),
        login_update,
        qr_expires: 0,
    };
    let _ = emit(&nonce, Event::Ready).await;
    // Authentication commands have their own idle deadline and every Telegram
    // RPC is bounded. Once a user starts a large, explicit snapshot, do not
    // terminate otherwise healthy work because of an arbitrary wall-clock cap.
    let outcome = guarded_run(connection.run(&mut commands), &mut cancel).await;
    let error = match outcome {
        RunOutcome::Failed(code) => Some(code),
        RunOutcome::Panicked => Some("telegram_protocol_changed"),
        _ => None,
    };
    if let Some(code) = error {
        let _ = emit(&nonce, Event::Error { code: code.into() }).await;
    }
    // Attempt remote revocation even if auth failed halfway through or SDK panicked.
    let confirmed = bounded_logout(
        async {
            match client.sign_out().await {
                Ok(_) => true,
                Err(error) => error.is("AUTH_KEY_UNREGISTERED"),
            }
        },
        Duration::from_secs(6),
    )
    .await;
    let _ = emit(&nonce, Event::LoggedOut { confirmed }).await;
    client.disconnect();
    reader.abort();
    update_task.abort();
    runner.abort();
    // No session serialization; process exit releases all SDK/session allocations.
}
#[derive(Debug, PartialEq, Eq)]
enum RunOutcome {
    Done,
    Failed(&'static str),
    Panicked,
    Cancelled,
}
async fn guarded_run(
    future: impl std::future::Future<Output = Result<(), &'static str>>,
    cancel: &mut watch::Receiver<bool>,
) -> RunOutcome {
    if *cancel.borrow() {
        return RunOutcome::Cancelled;
    }
    tokio::select! {
        result=std::panic::AssertUnwindSafe(future).catch_unwind()=>match result{Ok(Ok(()))=>RunOutcome::Done,Ok(Err(code))=>RunOutcome::Failed(code),Err(_)=>RunOutcome::Panicked},
        _=cancel.changed()=>RunOutcome::Cancelled,
    }
}
async fn bounded_logout(
    future: impl std::future::Future<Output = bool>,
    deadline: Duration,
) -> bool {
    // SDK cleanup can fail or panic independently of the import. Neither may
    // prevent the final unconfirmed-revocation event and process teardown.
    matches!(
        tokio::time::timeout(
            deadline,
            std::panic::AssertUnwindSafe(future).catch_unwind()
        )
        .await,
        Ok(Ok(true))
    )
}
pub async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    input: &mut R,
    bytes: &mut Vec<u8>,
) -> Result<(), &'static str> {
    loop {
        let buffer = input.fill_buf().await.map_err(|_| "host_disconnected")?;
        if buffer.is_empty() {
            return Err("host_disconnected");
        }
        let count = buffer
            .iter()
            .position(|b| *b == b'\n')
            .map_or(buffer.len(), |p| p + 1);
        if bytes.len() + count > MAX_FRAME {
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
fn safe_error(error: InvocationError) -> &'static str {
    if error.is("SESSION_PASSWORD_NEEDED") {
        "password_required"
    } else if error.is("PHONE_CODE_INVALID") || error.is("PHONE_CODE_EXPIRED") {
        "invalid_code"
    } else if error.is("PASSWORD_HASH_INVALID") {
        "invalid_password"
    } else if matches!(&error, InvocationError::Io(io) if io.kind() == std::io::ErrorKind::PermissionDenied)
    {
        // Windows reports a socket blocked by the process launch environment,
        // firewall or endpoint policy as WSAEACCES. Keep the OS details private
        // while distinguishing this from an ordinary transient outage.
        "telegram_network_blocked"
    } else if matches!(&error,InvocationError::Rpc(e) if e.code==420) {
        "telegram_rate_limit"
    } else if error.is("AUTH_KEY_UNREGISTERED") || error.is("SESSION_REVOKED") {
        "session_expired"
    } else {
        "telegram_unavailable"
    }
}
fn bounded_label(value: &str, max_bytes: usize) -> String {
    let mut end = 0;
    for (index, character) in value.char_indices() {
        let next = index + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    value[..end].to_string()
}
struct Connection {
    client: Client,
    session: Arc<MemorySession>,
    api_id: i32,
    api_hash: &'static str,
    nonce: String,
    phone: Option<Zeroizing<String>>,
    code_hash: Option<Zeroizing<String>>,
    account: Option<i64>,
    dialogs: HashMap<i64, grammers_client::peer::Dialog>,
    topics: HashMap<i64, Vec<i32>>,
    login_update: Arc<AtomicBool>,
    qr_expires: i32,
}
impl Connection {
    async fn rpc<T: tl::RemoteCall>(&self, request: &T) -> Result<T::Return, &'static str> {
        tokio::time::timeout(Duration::from_secs(65), self.client.invoke(request))
            .await
            .map_err(|_| "telegram_timeout")?
            .map_err(safe_error)
    }
    async fn authorized(&mut self) -> Result<(), &'static str> {
        let me = tokio::time::timeout(Duration::from_secs(30), self.client.get_me())
            .await
            .map_err(|_| "telegram_timeout")?
            .map_err(safe_error)?;
        self.account = me.id().bot_api_dialog_id();
        let account_id = self.account.ok_or("invalid_account")?;
        emit(
            &self.nonce,
            Event::Authorized {
                account_id,
                name: bounded_label(&me.full_name(), 256),
            },
        )
        .await
    }
    async fn qr(&mut self) -> Result<(), &'static str> {
        let result = self
            .rpc(&tl::functions::auth::ExportLoginToken {
                api_id: self.api_id,
                api_hash: self.api_hash.into(),
                except_ids: vec![],
            })
            .await;
        match result {
            Ok(tl::enums::auth::LoginToken::Token(token)) => {
                self.qr_expires = token.expires;
                let url = Zeroizing::new(format!(
                    "tg://login?token={}",
                    URL_SAFE_NO_PAD.encode(&token.token)
                ));
                let qr = qrcode::QrCode::new(url.as_bytes()).map_err(|_| "qr_unavailable")?;
                let svg = qr
                    .render::<qrcode::render::svg::Color>()
                    .quiet_zone(true)
                    .min_dimensions(240, 240)
                    .build();
                emit(
                    &self.nonce,
                    Event::Qr {
                        svg,
                        expires: token.expires,
                    },
                )
                .await
            }
            Ok(tl::enums::auth::LoginToken::Success(_)) => self.authorized().await,
            Ok(tl::enums::auth::LoginToken::MigrateTo(token)) => {
                if !(1..=5).contains(&token.dc_id) {
                    return Err("telegram_protocol_changed");
                }
                self.session
                    .set_home_dc_id(token.dc_id)
                    .await
                    .map_err(|_| "telegram_unavailable")?;
                let result = self
                    .rpc(&tl::functions::auth::ImportLoginToken { token: token.token })
                    .await;
                match result {
                    Ok(tl::enums::auth::LoginToken::Success(_)) => self.authorized().await,
                    Err("password_required") => emit(&self.nonce, Event::PasswordRequired).await,
                    _ => Err("qr_expired"),
                }
            }
            Err("password_required") => emit(&self.nonce, Event::PasswordRequired).await,
            Err(code) => Err(code),
        }
    }
    async fn phone(&mut self, phone: &str) -> Result<(), &'static str> {
        if self.account.is_some()
            || phone.len() < 7
            || phone.len() > 24
            || !phone
                .chars()
                .enumerate()
                .all(|(i, c)| c.is_ascii_digit() || (i == 0 && c == '+'))
        {
            return Err("invalid_phone");
        }
        let request = tl::functions::auth::SendCode {
            phone_number: phone.into(),
            api_id: self.api_id,
            api_hash: self.api_hash.into(),
            settings: tl::types::CodeSettings {
                allow_flashcall: false,
                current_number: false,
                allow_app_hash: false,
                allow_missed_call: false,
                allow_firebase: false,
                unknown_number: false,
                logout_tokens: None,
                token: None,
                app_sandbox: None,
            }
            .into(),
        };
        // Telegram can migrate phone authorization to another DC.
        let response =
            match tokio::time::timeout(Duration::from_secs(65), self.client.invoke(&request))
                .await
                .map_err(|_| "telegram_timeout")?
            {
                Err(InvocationError::Rpc(e)) if e.code == 303 => {
                    let dc = e
                        .value
                        .filter(|id| (1..=5).contains(id))
                        .ok_or("telegram_protocol_changed")?;
                    self.session
                        .set_home_dc_id(dc as i32)
                        .await
                        .map_err(|_| "telegram_unavailable")?;
                    self.rpc(&request).await?
                }
                result => result.map_err(safe_error)?,
            };
        match response {
            tl::enums::auth::SentCode::Code(code) => {
                self.phone = Some(Zeroizing::new(phone.into()));
                self.code_hash = Some(Zeroizing::new(code.phone_code_hash));
                emit(&self.nonce, Event::CodeRequired).await
            }
            tl::enums::auth::SentCode::Success(_) => self.authorized().await,
            tl::enums::auth::SentCode::PaymentRequired(_) => Err("telegram_requires_official_app"),
        }
    }
    async fn code(&mut self, code: &str) -> Result<(), &'static str> {
        if code.is_empty() || code.len() > 12 || !code.chars().all(|c| c.is_ascii_digit()) {
            return Err("invalid_code");
        }
        let request = tl::functions::auth::SignIn {
            phone_number: self.phone.as_ref().ok_or("code_not_requested")?.to_string(),
            phone_code_hash: self
                .code_hash
                .as_ref()
                .ok_or("code_not_requested")?
                .to_string(),
            phone_code: Some(code.into()),
            email_verification: None,
        };
        match self.rpc(&request).await {
            Ok(tl::enums::auth::Authorization::Authorization(_)) => {
                self.phone = None;
                self.code_hash = None;
                self.authorized().await
            }
            Ok(tl::enums::auth::Authorization::SignUpRequired(_)) => {
                Err("telegram_requires_official_app")
            }
            Err("password_required") => emit(&self.nonce, Event::PasswordRequired).await,
            Err(e) => Err(e),
        }
    }
    async fn password(&mut self, password: &str) -> Result<(), &'static str> {
        if password.is_empty() || password.len() > 1024 {
            return Err("invalid_password");
        }
        let tl::enums::account::Password::Password(info) =
            self.rpc(&tl::functions::account::GetPassword {}).await?;
        let supported = match info.current_algo.as_ref() {
            Some(
                tl::enums::PasswordKdfAlgo::Sha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow(a),
            ) => a.p.len() == 256 && grammers_crypto::two_factor_auth::check_p_and_g(&a.p, &a.g),
            _ => false,
        };
        if !supported
            || info
                .srp_b
                .as_ref()
                .is_none_or(|b| b.is_empty() || b.len() > 256)
            || info.srp_id.is_none()
        {
            return Err("telegram_protocol_changed");
        }
        tokio::time::timeout(
            Duration::from_secs(65),
            self.client
                .check_password(PasswordToken::new(info), password.as_bytes()),
        )
        .await
        .map_err(|_| "telegram_timeout")?
        .map_err(|_| "invalid_password")?;
        self.phone = None;
        self.code_hash = None;
        self.authorized().await
    }
    async fn catalog(&mut self) -> Result<(), &'static str> {
        if self.account.is_none() {
            return Err("login_required");
        }
        self.dialogs.clear();
        let mut output = vec![];
        let mut iter = self.client.iter_dialogs();
        while let Some(dialog) = tokio::time::timeout(Duration::from_secs(65), iter.next())
            .await
            .map_err(|_| "telegram_timeout")?
            .map_err(safe_error)?
        {
            let id = dialog
                .peer_id()
                .bot_api_dialog_id()
                .ok_or("invalid_dialog")?;
            let (available, forum) = match dialog.peer() {
                grammers_client::peer::Peer::Group(g) => match &g.raw {
                    tl::enums::Chat::Chat(c) => (!c.noforwards, false),
                    tl::enums::Chat::Channel(c) => (!c.noforwards && !c.left, c.forum),
                    _ => (false, false),
                },
                grammers_client::peer::Peer::Channel(c) => {
                    (!c.raw.noforwards && !c.raw.left, c.raw.forum)
                }
                grammers_client::peer::Peer::User(u) => (!u.deleted(), false),
            };
            output.push(Dialog {
                id,
                title: bounded_label(dialog.peer().name().unwrap_or("Unnamed chat"), 256),
                available,
                forum,
            });
            self.dialogs.insert(id, dialog);
            if output.len() == MAX_DIALOGS {
                break;
            }
        }
        let truncated = output.len() == MAX_DIALOGS;
        emit(
            &self.nonce,
            Event::Catalog {
                dialogs: output,
                truncated,
            },
        )
        .await
    }
    async fn topics(&mut self, chat_id: i64) -> Result<(), &'static str> {
        let dialog = self
            .dialogs
            .get(&chat_id)
            .ok_or("invalid_selection")?
            .clone();
        let peer: tl::enums::InputPeer = dialog.peer_ref().into();
        let mut offset_date = 0;
        let mut offset_id = 0;
        let mut offset_topic = 0;
        let mut result = vec![];
        loop {
            let tl::enums::messages::ForumTopics::Topics(page) = self
                .rpc(&tl::functions::messages::GetForumTopics {
                    peer: peer.clone(),
                    q: None,
                    offset_date,
                    offset_id,
                    offset_topic,
                    limit: 100,
                })
                .await?;
            if page.topics.is_empty() {
                break;
            }
            let mut last = None;
            for topic in page.topics {
                if let tl::enums::ForumTopic::Topic(topic) = topic {
                    let date = if page.order_by_create_date {
                        topic.date
                    } else {
                        page.messages
                            .iter()
                            .find_map(|m| match m {
                                tl::enums::Message::Message(m) if m.id == topic.top_message => {
                                    Some(m.date)
                                }
                                tl::enums::Message::Service(m) if m.id == topic.top_message => {
                                    Some(m.date)
                                }
                                _ => None,
                            })
                            .ok_or("topic_cursor_stalled")?
                    };
                    last = Some((date, topic.top_message, topic.id));
                    if !result.iter().any(|t: &Topic| t.id == topic.id) {
                        result.push(Topic {
                            id: topic.id,
                            title: bounded_label(&topic.title, 256),
                        });
                    }
                }
            }
            let next = last.ok_or("topic_cursor_stalled")?;
            if next == (offset_date, offset_id, offset_topic) {
                return Err("topic_cursor_stalled");
            }
            (offset_date, offset_id, offset_topic) = next;
            if result.len() >= MAX_DIALOGS || result.len() >= page.count.max(0) as usize {
                break;
            }
        }
        let truncated = result.len() >= MAX_DIALOGS;
        self.topics
            .insert(chat_id, result.iter().map(|t| t.id).collect());
        emit(
            &self.nonce,
            Event::Topics {
                chat_id,
                topics: result,
                truncated,
            },
        )
        .await
    }
    async fn preflight(&self, peer: PeerRef) -> Result<(), &'static str> {
        let input: tl::enums::InputPeer = peer.into();
        let ttl = match input {
            tl::enums::InputPeer::Chat(p) => {
                let full = self
                    .rpc(&tl::functions::messages::GetFullChat { chat_id: p.chat_id })
                    .await?;
                let tl::enums::messages::ChatFull::Full(full) = full;
                if !full.chats.iter().any(|c| chat_readable(c, p.chat_id)) {
                    return Err("protected_chat_excluded");
                }
                match full.full_chat {
                    tl::enums::ChatFull::Full(c) => c.ttl_period,
                    tl::enums::ChatFull::ChannelFull(c) => c.ttl_period,
                }
            }
            tl::enums::InputPeer::Channel(p) => {
                let full = self
                    .rpc(&tl::functions::channels::GetFullChannel {
                        channel: tl::types::InputChannel {
                            channel_id: p.channel_id,
                            access_hash: p.access_hash,
                        }
                        .into(),
                    })
                    .await?;
                let tl::enums::messages::ChatFull::Full(full) = full;
                if !full.chats.iter().any(|c| chat_readable(c, p.channel_id)) {
                    return Err("protected_chat_excluded");
                }
                match full.full_chat {
                    tl::enums::ChatFull::Full(c) => c.ttl_period,
                    tl::enums::ChatFull::ChannelFull(c) => c.ttl_period,
                }
            }
            tl::enums::InputPeer::User(p) => {
                let full = self
                    .rpc(&tl::functions::users::GetFullUser {
                        id: tl::types::InputUser {
                            user_id: p.user_id,
                            access_hash: p.access_hash,
                        }
                        .into(),
                    })
                    .await?;
                let tl::enums::users::UserFull::Full(full) = full;
                let tl::enums::UserFull::Full(user) = full.full_user;
                user.ttl_period
            }
            _ => return Err("unsupported_chat"),
        };
        if ttl.is_some_and(|t| t > 0) {
            return Err("auto_delete_chat_excluded");
        }
        Ok(())
    }
    async fn export(
        &mut self,
        scope: &Scope,
        cursors: &[Cursor],
        expected: Option<i64>,
        commands: &mut mpsc::Receiver<Command>,
    ) -> Result<(), &'static str> {
        scope.validate(chrono::Utc::now().timestamp())?;
        let account = self.account.ok_or("login_required")?;
        if expected.is_some_and(|id| id != account) {
            return Err("resume_identity_mismatch");
        }
        // Validate every selected peer before downloading the first page. One
        // protected, inaccessible or auto-delete chat must not create a
        // misleading partially advanced multi-chat snapshot.
        let mut planned = Vec::with_capacity(scope.chats.len());
        for selected in &scope.chats {
            let dialog = self
                .dialogs
                .get(&selected.chat_id)
                .ok_or("invalid_selection")?
                .clone();
            if selected.topic_id.is_some() && !self.topics.contains_key(&selected.chat_id) {
                self.topics(selected.chat_id).await?;
            }
            if selected.topic_id.is_some_and(|id| {
                !self
                    .topics
                    .get(&selected.chat_id)
                    .is_some_and(|topics| topics.contains(&id))
            }) {
                return Err("invalid_selection");
            }
            // Re-read protection/TTL at export time; every message is filtered too.
            self.preflight(dialog.peer_ref()).await?;
            planned.push((selected.clone(), dialog));
        }
        let mut page_totals = Vec::with_capacity(planned.len());
        for (selected, dialog) in &planned {
            let cursor = cursors
                .iter()
                .find(|c| c.chat_id == selected.chat_id && c.topic_id == selected.topic_id)
                .cloned()
                .unwrap_or(Cursor {
                    chat_id: selected.chat_id,
                    topic_id: selected.topic_id,
                    ..Default::default()
                });
            let count_request = tl::functions::messages::Search {
                peer: dialog.peer_ref().into(),
                q: String::new(),
                from_id: None,
                saved_peer_id: None,
                saved_reaction: None,
                top_msg_id: selected.topic_id,
                filter: tl::enums::MessagesFilter::InputMessagesFilterEmpty,
                min_date: scope.from,
                max_date: scope.until,
                offset_id: 0,
                add_offset: 0,
                limit: 1,
                max_id: if cursor.high_water > 0 {
                    cursor.high_water.saturating_add(1)
                } else {
                    0
                },
                min_id: 0,
                hash: 0,
            };
            let total =
                match tokio::time::timeout(Duration::from_secs(10), self.rpc(&count_request)).await
                {
                    Ok(Ok(response)) => history_count(&response)
                        .ok()
                        // The exporter asks Telegram for 100 records at a time.
                        // One final page observes the lower date boundary.
                        .and_then(|messages| messages.checked_div(100)?.checked_add(1)),
                    _ => None,
                };
            page_totals.push(total);
        }
        let total_chats = planned.len();
        let mut completed_chats = cursors.iter().filter(|cursor| cursor.done).count();
        for (index, (selected, dialog)) in planned.into_iter().enumerate() {
            let mut cursor = cursors
                .iter()
                .find(|c| c.chat_id == selected.chat_id && c.topic_id == selected.topic_id)
                .cloned()
                .unwrap_or(Cursor {
                    chat_id: selected.chat_id,
                    topic_id: selected.topic_id,
                    ..Default::default()
                });
            if cursor.done {
                continue;
            }
            emit(
                &self.nonce,
                Event::Progress {
                    total_chats,
                    completed_chats,
                    current_chat: index + 1,
                    current_pages: cursor.pages,
                    total_pages: page_totals.get(index).copied().flatten(),
                },
            )
            .await?;
            let title = bounded_label(dialog.peer().name().unwrap_or("Unnamed chat"), 256);
            loop {
                let peer: tl::enums::InputPeer = dialog.peer_ref().into();
                let response = if let Some(topic) = selected.topic_id {
                    self.rpc(&tl::functions::messages::GetReplies {
                        peer,
                        msg_id: topic,
                        offset_id: cursor.offset_id,
                        offset_date: if cursor.offset_id == 0 {
                            scope.until
                        } else {
                            0
                        },
                        add_offset: 0,
                        limit: 100,
                        max_id: if cursor.high_water > 0 {
                            cursor.high_water.saturating_add(1)
                        } else {
                            0
                        },
                        min_id: 0,
                        hash: 0,
                    })
                    .await?
                } else {
                    self.rpc(&tl::functions::messages::GetHistory {
                        peer,
                        offset_id: cursor.offset_id,
                        offset_date: if cursor.offset_id == 0 {
                            scope.until
                        } else {
                            0
                        },
                        add_offset: 0,
                        limit: 100,
                        max_id: if cursor.high_water > 0 {
                            cursor.high_water.saturating_add(1)
                        } else {
                            0
                        },
                        min_id: 0,
                        hash: 0,
                    })
                    .await?
                };
                let (raw, users, chats) = unpack(response)?;
                let mut names = HashMap::new();
                for user in users {
                    if let tl::enums::User::User(u) = user {
                        names.insert(
                            u.id,
                            format!(
                                "{} {}",
                                u.first_name.unwrap_or_default(),
                                u.last_name.unwrap_or_default()
                            )
                            .trim()
                            .chars()
                            .take(256)
                            .collect::<String>(),
                        );
                    }
                }
                for chat in chats {
                    match chat {
                        tl::enums::Chat::Chat(c) => {
                            names.insert(-c.id, c.title);
                        }
                        tl::enums::Chat::Channel(c) => {
                            names.insert(-1_000_000_000_000 - c.id, c.title);
                        }
                        _ => {}
                    }
                }
                let mut messages =
                    consume_page(&raw, &mut cursor, scope, &selected, account, &names)?;
                if cursor.done {
                    if let Some(topic) = selected.topic_id {
                        // getReplies returns a thread; include only its selected root.
                        let peer: tl::enums::InputPeer = dialog.peer_ref().into();
                        let ids = vec![tl::types::InputMessageId { id: topic }.into()];
                        let response = match peer {
                            tl::enums::InputPeer::Channel(p) => {
                                self.rpc(&tl::functions::channels::GetMessages {
                                    channel: tl::types::InputChannel {
                                        channel_id: p.channel_id,
                                        access_hash: p.access_hash,
                                    }
                                    .into(),
                                    id: ids,
                                })
                                .await?
                            }
                            _ => {
                                self.rpc(&tl::functions::messages::GetMessages { id: ids })
                                    .await?
                            }
                        };
                        let (roots, users, _) = unpack(response)?;
                        for user in users {
                            if let tl::enums::User::User(u) = user {
                                names.insert(
                                    u.id,
                                    format!(
                                        "{} {}",
                                        u.first_name.unwrap_or_default(),
                                        u.last_name.unwrap_or_default()
                                    )
                                    .trim()
                                    .chars()
                                    .take(256)
                                    .collect(),
                                );
                            }
                        }
                        for root in roots {
                            if let tl::enums::Message::Message(m) = root {
                                if m.id == topic {
                                    if let Some(message) = normalize(&m, account, &names) {
                                        if message.validate(scope, &selected)
                                            && !messages.iter().any(|old| old.id == topic)
                                        {
                                            cursor.high_water = cursor.high_water.max(topic);
                                            messages.push(message);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                cursor.pages = cursor.pages.checked_add(1).ok_or("protocol_error")?;
                let page_id = digest(
                    &serde_json::to_vec(&(account, &selected, &cursor, &messages))
                        .map_err(|_| "protocol_error")?,
                );
                emit(
                    &self.nonce,
                    Event::Page {
                        page_id: page_id.clone(),
                        account_id: account,
                        chat_id: selected.chat_id,
                        title: title.clone(),
                        cursor: cursor.clone(),
                        messages,
                    },
                )
                .await?;
                let ack = tokio::time::timeout(Duration::from_secs(30), commands.recv())
                    .await
                    .map_err(|_| "checkpoint_timeout")?
                    .ok_or("host_disconnected")?;
                if !matches!(&ack,Command::Ack{page_id:id} if id==&page_id) {
                    return Err("checkpoint_mismatch");
                }
                if cursor.done {
                    completed_chats += 1;
                    break;
                }
            }
        }
        emit(&self.nonce, Event::Complete).await
    }
    async fn run(&mut self, commands: &mut mpsc::Receiver<Command>) -> Result<(), &'static str> {
        while let Some(command) =
            tokio::time::timeout(Duration::from_secs(10 * 60), commands.recv())
                .await
                .map_err(|_| "login_expired")?
        {
            let result = match &command {
                Command::Qr if self.account.is_none() => self.qr().await,
                Command::PollQr => {
                    if self.account.is_none()
                        && (self.login_update.swap(false, Ordering::SeqCst)
                            || chrono::Utc::now().timestamp() >= i64::from(self.qr_expires))
                    {
                        self.qr().await
                    } else {
                        Ok(())
                    }
                }
                Command::Phone { phone } => self.phone(phone).await,
                Command::Code { code } => self.code(code).await,
                Command::Password { password } => self.password(password).await,
                Command::Catalog => self.catalog().await,
                Command::Topics { chat_id } => self.topics(*chat_id).await,
                Command::Export {
                    scope,
                    cursors,
                    account_id,
                } => {
                    self.export(scope, cursors, *account_id, commands).await?;
                    return Ok(());
                }
                Command::Cancel => return Ok(()),
                _ => Err("invalid_state"),
            };
            if let Err(code) = result {
                emit(&self.nonce, Event::Error { code: code.into() }).await?;
            }
        }
        Ok(())
    }
}
fn has_login_update(update: grammers_session::updates::UpdatesLike) -> bool {
    let grammers_session::updates::UpdatesLike::Updates(update) = update else {
        return false;
    };
    match update {
        tl::enums::Updates::UpdateShort(short) => {
            matches!(short.update, tl::enums::Update::LoginToken)
        }
        tl::enums::Updates::Updates(u) => u
            .updates
            .iter()
            .any(|u| matches!(u, tl::enums::Update::LoginToken)),
        tl::enums::Updates::Combined(u) => u
            .updates
            .iter()
            .any(|u| matches!(u, tl::enums::Update::LoginToken)),
        _ => false,
    }
}
type HistoryPage = (
    Vec<tl::enums::Message>,
    Vec<tl::enums::User>,
    Vec<tl::enums::Chat>,
);
fn history_count(response: &tl::enums::messages::Messages) -> Result<usize, &'static str> {
    use tl::enums::messages::Messages;
    let count = match response {
        Messages::Messages(page) => page.messages.len() as i32,
        Messages::Slice(page) => page.count,
        Messages::ChannelMessages(page) => page.count,
        Messages::NotModified(page) => page.count,
    };
    usize::try_from(count).map_err(|_| "telegram_protocol_changed")
}
fn unpack(response: tl::enums::messages::Messages) -> Result<HistoryPage, &'static str> {
    use tl::enums::messages::Messages;
    match response {
        Messages::Messages(m) => Ok((m.messages, m.users, m.chats)),
        Messages::Slice(m) => Ok((m.messages, m.users, m.chats)),
        Messages::ChannelMessages(m) => Ok((m.messages, m.users, m.chats)),
        Messages::NotModified(_) => Err("telegram_protocol_changed"),
    }
}
fn chat_readable(chat: &tl::enums::Chat, id: i64) -> bool {
    match chat {
        tl::enums::Chat::Chat(c) => c.id == id && !c.noforwards,
        tl::enums::Chat::Channel(c) => c.id == id && !c.noforwards && !c.left,
        _ => false,
    }
}
fn message_id(message: &tl::enums::Message) -> i32 {
    match message {
        tl::enums::Message::Message(m) => m.id,
        tl::enums::Message::Service(m) => m.id,
        tl::enums::Message::Empty(m) => m.id,
    }
}
fn consume_page(
    raw: &[tl::enums::Message],
    cursor: &mut Cursor,
    scope: &Scope,
    selected: &Selection,
    account: i64,
    names: &HashMap<i64, String>,
) -> Result<Vec<Message>, &'static str> {
    let previous = cursor.offset_id;
    let mut min_id = i32::MAX;
    let mut older = false;
    let mut messages = vec![];
    if cursor.high_water == 0 {
        cursor.high_water = raw.iter().map(message_id).max().unwrap_or(0).max(0);
    }
    for item in raw {
        let id = message_id(item);
        if id <= 0 || (previous > 0 && id >= previous) || id > cursor.high_water {
            continue;
        }
        min_id = min_id.min(id);
        if let tl::enums::Message::Message(m) = item {
            older |= m.date < scope.from;
            if let Some(message) = normalize(m, account, names) {
                if message.validate(scope, selected) {
                    messages.push(message);
                }
            }
        }
    }
    if raw.is_empty() {
        cursor.done = true;
        cursor.offset_id = if previous > 0 {
            previous.saturating_sub(1)
        } else {
            0
        };
    } else if min_id == i32::MAX {
        return Err("history_cursor_stalled");
    } else {
        cursor.offset_id = min_id;
        cursor.done = older || min_id <= 1;
    }
    Ok(messages)
}
fn normalize(
    m: &tl::types::Message,
    account: i64,
    names: &HashMap<i64, String>,
) -> Option<Message> {
    if m.noforwards || m.ttl_period.is_some_and(|t| t > 0) || m.message.trim().is_empty() {
        return None;
    }
    let sender = m
        .from_id
        .as_ref()
        .map(|p| PeerId::from(p).bot_api_dialog_id_unchecked())
        .unwrap_or(if m.out {
            account
        } else {
            PeerId::from(&m.peer_id).bot_api_dialog_id_unchecked()
        });
    let forward = m
        .fwd_from
        .as_ref()
        .map(|tl::enums::MessageFwdHeader::Header(f)| f);
    let author_id = forward
        .and_then(|f| f.from_id.as_ref())
        .map(|p| PeerId::from(p).bot_api_dialog_id_unchecked());
    let author = forward
        .map(|f| {
            f.from_name
                .clone()
                .or_else(|| author_id.and_then(|id| names.get(&id).cloned()))
                .unwrap_or("Forwarded author".into())
        })
        .unwrap_or_else(|| {
            names
                .get(&sender)
                .cloned()
                .unwrap_or("Unknown author".into())
        });
    let author = bounded_label(&author, 1024);
    let (reply, topic) = match m.reply_to.as_ref() {
        Some(tl::enums::MessageReplyHeader::Header(r)) => (
            r.reply_to_msg_id,
            if r.forum_topic {
                r.reply_to_top_id.or(r.reply_to_msg_id)
            } else {
                None
            },
        ),
        _ => (None, None),
    };
    Some(Message {
        id: m.id,
        date: m.date,
        author_id: if forward.is_some() {
            author_id.map_or("forwarded_unknown".into(), |id| format!("user{id}"))
        } else {
            format!("user{sender}")
        },
        author,
        text: m.message.clone(),
        reply_to: reply,
        topic_id: topic,
        edited_at: m.edit_date,
        forwarded_from: forward.map(|f| {
            let value = f
                .from_name
                .clone()
                .unwrap_or_else(|| author_id.map_or("unknown".into(), |id| id.to_string()));
            bounded_label(&value, 1024)
        }),
        forwarded_date: forward.map(|f| f.date),
        self_authored: sender == account && forward.is_none(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalogue_labels_are_utf8_safe_and_frame_bounded() {
        let label = bounded_label(&"🧠".repeat(100), 256);
        assert_eq!(label.len(), 256);
        assert_eq!(label.chars().count(), 64);
    }
    #[tokio::test]
    async fn failures_and_sdk_panics_return_to_cleanup() {
        let (_tx, mut rx) = watch::channel(false);
        assert_eq!(
            guarded_run(async { Err("synthetic_failure") }, &mut rx).await,
            RunOutcome::Failed("synthetic_failure")
        );
        assert_eq!(
            guarded_run(async { panic!("synthetic SDK failure") }, &mut rx).await,
            RunOutcome::Panicked
        );
        assert!(bounded_logout(async { true }, Duration::from_secs(1)).await);
        assert!(
            !bounded_logout(
                async { panic!("synthetic logout failure") },
                Duration::from_secs(1)
            )
            .await
        );
    }
    #[tokio::test]
    async fn cancellation_does_not_cancel_independent_logout() {
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        assert_eq!(
            guarded_run(std::future::pending(), &mut rx).await,
            RunOutcome::Cancelled
        );
        assert!(bounded_logout(async { true }, Duration::from_secs(1)).await);
    }
    #[tokio::test]
    async fn logout_timeout_is_bounded() {
        assert!(!bounded_logout(std::future::pending(), Duration::from_millis(1)).await);
    }
    fn message(id: i32, date: i32) -> tl::types::Message {
        tl::types::Message {
            out: true,
            mentioned: false,
            media_unread: false,
            silent: false,
            post: false,
            from_scheduled: false,
            legacy: false,
            edit_hide: false,
            pinned: false,
            noforwards: false,
            invert_media: false,
            offline: false,
            video_processing_pending: false,
            paid_suggested_post_stars: false,
            paid_suggested_post_ton: false,
            id,
            from_id: Some(tl::types::PeerUser { user_id: 7 }.into()),
            from_boosts_applied: None,
            from_rank: None,
            peer_id: tl::types::PeerUser { user_id: 8 }.into(),
            saved_peer_id: None,
            fwd_from: None,
            via_bot_id: None,
            via_business_bot_id: None,
            guestchat_via_from: None,
            reply_to: None,
            date,
            message: "Synthetic: café, путешествие, walking by the river".into(),
            media: None,
            reply_markup: None,
            entities: None,
            views: None,
            forwards: None,
            replies: None,
            edit_date: None,
            post_author: None,
            grouped_id: None,
            reactions: None,
            restriction_reason: None,
            ttl_period: None,
            quick_reply_shortcut_id: None,
            effect: None,
            factcheck: None,
            report_delivery_until_date: None,
            paid_message_stars: None,
            suggested_post: None,
            schedule_repeat_period: None,
            summary_from_language: None,
            rich_message: None,
        }
    }
    #[test]
    fn excludes_protected_expiring_and_empty_messages() {
        let mut m = message(4, 50);
        assert!(normalize(&m, 7, &HashMap::new()).is_some());
        m.noforwards = true;
        assert!(normalize(&m, 7, &HashMap::new()).is_none());
        m.noforwards = false;
        m.ttl_period = Some(30);
        assert!(normalize(&m, 7, &HashMap::new()).is_none());
        m.ttl_period = None;
        m.message = " ".into();
        assert!(normalize(&m, 7, &HashMap::new()).is_none());
    }
    #[test]
    fn forwarded_text_keeps_original_author_instead_of_subject() {
        let mut m = message(4, 50);
        m.fwd_from = Some(
            tl::types::MessageFwdHeader {
                imported: false,
                saved_out: false,
                from_id: Some(tl::types::PeerUser { user_id: 99 }.into()),
                from_name: Some("Synthetic Friend".into()),
                date: 30,
                channel_post: None,
                post_author: None,
                saved_from_peer: None,
                saved_from_msg_id: None,
                saved_from_id: None,
                saved_from_name: None,
                saved_date: None,
                psa_type: None,
            }
            .into(),
        );
        let result = normalize(&m, 7, &HashMap::new()).unwrap();
        assert_eq!(result.author_id, "user99");
        assert_eq!(result.author, "Synthetic Friend");
        assert_eq!(result.forwarded_date, Some(30));
        assert!(!result.self_authored);
    }
    #[test]
    fn preserves_reply_topic_edit_and_multilingual_text() {
        let mut m = message(4, 50);
        m.edit_date = Some(60);
        m.reply_to = Some(
            tl::types::MessageReplyHeader {
                reply_to_scheduled: false,
                forum_topic: true,
                quote: false,
                reply_to_ephemeral: false,
                reply_to_msg_id: Some(3),
                reply_to_peer_id: None,
                reply_from: None,
                reply_media: None,
                reply_to_top_id: Some(2),
                quote_text: None,
                quote_entities: None,
                quote_offset: None,
                todo_item_id: None,
                poll_option: None,
            }
            .into(),
        );
        let result = normalize(&m, 7, &HashMap::from([(7, "Synthetic Owner".into())])).unwrap();
        assert_eq!(result.reply_to, Some(3));
        assert_eq!(result.topic_id, Some(2));
        assert_eq!(result.edited_at, Some(60));
        assert!(result.self_authored);
        assert_eq!(result.author, "Synthetic Owner");
        assert_eq!(result.text, m.message);
    }
    #[test]
    fn short_pages_continue_and_empty_pages_finish() {
        let scope = Scope {
            chats: vec![Selection {
                chat_id: 8,
                topic_id: None,
            }],
            from: 20,
            until: 100,
        };
        let mut cursor = Cursor::default();
        let page = vec![message(9, 90).into(), message(7, 70).into()];
        assert_eq!(
            consume_page(
                &page,
                &mut cursor,
                &scope,
                &scope.chats[0],
                7,
                &HashMap::new()
            )
            .unwrap()
            .len(),
            2
        );
        assert_eq!(cursor.offset_id, 7);
        assert_eq!(cursor.high_water, 9);
        assert!(!cursor.done);
        let page = vec![message(6, 60).into()];
        assert_eq!(
            consume_page(
                &page,
                &mut cursor,
                &scope,
                &scope.chats[0],
                7,
                &HashMap::new()
            )
            .unwrap()
            .len(),
            1
        );
        assert!(!cursor.done);
        consume_page(
            &[],
            &mut cursor,
            &scope,
            &scope.chats[0],
            7,
            &HashMap::new(),
        )
        .unwrap();
        assert!(cursor.done);
        assert_eq!(cursor.offset_id, 5);
    }
    #[test]
    fn stalled_cursor_fails_and_pinned_snapshot_excludes_new_messages() {
        let scope = Scope {
            chats: vec![Selection {
                chat_id: 8,
                topic_id: None,
            }],
            from: 20,
            until: 100,
        };
        let mut cursor = Cursor {
            offset_id: 7,
            high_water: 9,
            ..Default::default()
        };
        assert_eq!(
            consume_page(
                &[message(7, 70).into()],
                &mut cursor,
                &scope,
                &scope.chats[0],
                7,
                &HashMap::new()
            ),
            Err("history_cursor_stalled")
        );
        let result = consume_page(
            &[
                message(10, 90).into(),
                message(6, 60).into(),
                message(4, 10).into(),
            ],
            &mut cursor,
            &scope,
            &scope.chats[0],
            7,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, 6);
        assert!(cursor.done);
        assert_eq!(cursor.high_water, 9);
    }
    #[tokio::test]
    async fn bounds_unterminated_input() {
        let data = vec![b'x'; MAX_FRAME + 1];
        let mut reader = BufReader::new(data.as_slice());
        assert_eq!(
            read_frame(&mut reader, &mut vec![]).await,
            Err("invalid_frame")
        );
    }
    #[tokio::test]
    async fn reads_exactly_one_frame() {
        let mut reader = BufReader::new(b"one\ntwo\n".as_slice());
        let mut frame = vec![];
        read_frame(&mut reader, &mut frame).await.unwrap();
        assert_eq!(frame, b"one\n");
    }
    #[test]
    fn credentials_are_validated() {
        let hash = "a".repeat(32);
        assert!(parse_credentials("12345", &hash).is_some());
        for id in ["", "0", "-1", "x", " 12345", "999999999999999"] {
            assert!(parse_credentials(id, &hash).is_none());
        }
        for hash in [
            "",
            "abcd",
            "gggggggggggggggggggggggggggggggg",
            "🔑🔑🔑🔑🔑🔑🔑🔑",
        ] {
            assert!(parse_credentials("12345", hash).is_none());
        }
    }
    #[test]
    fn blocked_socket_has_actionable_safe_error() {
        let error = InvocationError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert_eq!(safe_error(error), "telegram_network_blocked");
    }
}
