use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub const MAX_FRAME: usize = 2 * 1024 * 1024;
pub const MAX_CHATS: usize = 20;
pub const MAX_DIALOGS: usize = 2000;
pub const MAX_SOURCE: usize = 8 * 1024 * 1024;

// Secrets deliberately have no Debug implementation. Never log a Command.
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Qr,
    PollQr,
    Phone {
        phone: String,
    },
    Code {
        code: String,
    },
    Password {
        password: String,
    },
    Catalog,
    Topics {
        chat_id: i64,
    },
    Export {
        scope: Scope,
        cursors: Vec<Cursor>,
        account_id: Option<i64>,
    },
    Ack {
        page_id: String,
    },
    Cancel,
}
impl Drop for Command {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        match self {
            Self::Phone { phone } => phone.zeroize(),
            Self::Code { code } => code.zeroize(),
            Self::Password { password } => password.zeroize(),
            _ => {}
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope<T> {
    pub version: u32,
    pub nonce: String,
    pub payload: T,
}
pub fn decode_command(bytes: &[u8], nonce: &str) -> Result<Command, &'static str> {
    if bytes.len() > MAX_FRAME || nonce.len() != 32 {
        return Err("invalid_frame");
    }
    let envelope: Envelope<serde_json::Value> =
        serde_json::from_slice(bytes).map_err(|_| "invalid_frame")?;
    if envelope.version != crate::PROTOCOL_VERSION || envelope.nonce != nonce {
        return Err("invalid_frame");
    }
    parse_command(envelope.payload)
}
pub fn parse_command(value: serde_json::Value) -> Result<Command, &'static str> {
    let object = value.as_object().ok_or("invalid_frame")?;
    let fields: &[&str] = match object
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or("invalid_frame")?
    {
        "qr" | "poll_qr" | "catalog" | "cancel" => &["type"],
        "phone" => &["type", "phone"],
        "code" => &["type", "code"],
        "password" => &["type", "password"],
        "topics" => &["type", "chat_id"],
        "ack" => &["type", "page_id"],
        "export" => &["type", "scope", "cursors", "account_id"],
        _ => return Err("invalid_frame"),
    };
    if object.keys().any(|key| !fields.contains(&key.as_str())) {
        return Err("invalid_frame");
    }
    serde_json::from_value(value).map_err(|_| "invalid_frame")
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub chat_id: i64,
    pub topic_id: Option<i32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub chats: Vec<Selection>,
    pub from: i32,
    pub until: i32,
}
impl Scope {
    pub fn validate(&self, now: i64) -> Result<(), &'static str> {
        if self.chats.is_empty()
            || self.chats.len() > MAX_CHATS
            || self.from < 0
            || self.until <= self.from
            || i64::from(self.until) > now + 300
        {
            return Err("invalid_selection");
        }
        let mut seen = std::collections::HashSet::new();
        for selection in &self.chats {
            if selection.chat_id == 0
                || selection.topic_id.is_some_and(|id| id <= 0)
                || !seen.insert((selection.chat_id, selection.topic_id))
            {
                return Err("invalid_selection");
            }
        }
        if self.chats.iter().any(|s| {
            s.topic_id.is_none()
                && self
                    .chats
                    .iter()
                    .any(|other| other.chat_id == s.chat_id && other.topic_id.is_some())
        }) {
            return Err("invalid_selection");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub chat_id: i64,
    pub topic_id: Option<i32>,
    pub offset_id: i32,
    pub high_water: i32,
    pub done: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pages: usize,
}
fn is_zero(value: &usize) -> bool {
    *value == 0
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dialog {
    pub id: i64,
    pub title: String,
    pub forum: bool,
    pub available: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Topic {
    pub id: i32,
    pub title: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub id: i32,
    pub date: i32,
    pub author_id: String,
    pub author: String,
    pub text: String,
    pub reply_to: Option<i32>,
    pub topic_id: Option<i32>,
    pub edited_at: Option<i32>,
    pub forwarded_from: Option<String>,
    pub forwarded_date: Option<i32>,
    pub self_authored: bool,
}
impl Message {
    pub fn validate(&self, scope: &Scope, selection: &Selection) -> bool {
        self.id > 0
            && self.date >= scope.from
            && self.date < scope.until
            && !self.text.trim().is_empty()
            && self.text.len() <= 32_768
            && self.author.len() <= 1024
            && self.author_id.len() <= 128
            && self.forwarded_from.as_ref().is_none_or(|v| v.len() <= 1024)
            && selection
                .topic_id
                .is_none_or(|id| self.topic_id == Some(id) || self.id == id)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    Ready,
    Qr {
        svg: String,
        expires: i32,
    },
    CodeRequired,
    PasswordRequired,
    Authorized {
        account_id: i64,
        name: String,
    },
    Catalog {
        dialogs: Vec<Dialog>,
        truncated: bool,
    },
    Topics {
        chat_id: i64,
        topics: Vec<Topic>,
        truncated: bool,
    },
    Progress {
        total_chats: usize,
        completed_chats: usize,
        current_chat: usize,
        current_pages: usize,
        total_pages: Option<usize>,
    },
    Page {
        page_id: String,
        account_id: i64,
        chat_id: i64,
        title: String,
        cursor: Cursor,
        messages: Vec<Message>,
    },
    Complete,
    Error {
        code: String,
    },
    LoggedOut {
        confirmed: bool,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub user_id: Uuid,
    pub persona_id: Uuid,
    pub device_id: Uuid,
    pub persona_name: String,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_foreign_nonce_version_and_extra_fields() {
        let nonce = "a".repeat(32);
        for input in [
            r#"{"version":1,"nonce":"wrong","payload":{"type":"cancel"}}"#,
            r#"{"version":2,"nonce":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","payload":{"type":"cancel"}}"#,
            r#"{"version":1,"nonce":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","payload":{"type":"cancel","rpc":"sendMessage"}}"#,
        ] {
            assert!(decode_command(input.as_bytes(), &nonce).is_err());
        }
        let good = serde_json::to_vec(&Envelope {
            version: 1,
            nonce: nonce.clone(),
            payload: Command::Cancel,
        })
        .unwrap();
        assert!(matches!(decode_command(&good, &nonce), Ok(Command::Cancel)));
        assert!(decode_command(&vec![b' '; MAX_FRAME + 1], &nonce).is_err());
    }
    #[test]
    fn validates_exact_selection_and_date_bounds() {
        let mut scope = Scope {
            chats: vec![Selection {
                chat_id: 42,
                topic_id: None,
            }],
            from: 1,
            until: 100,
        };
        assert!(scope.validate(100).is_ok());
        scope.chats.push(scope.chats[0].clone());
        assert!(scope.validate(100).is_err());
        scope.chats.pop();
        scope.until = 1000;
        assert!(scope.validate(100).is_err());
        scope.until = 1;
        assert!(scope.validate(100).is_err());
    }
    #[test]
    fn cursor_page_progress_preserves_old_checkpoint_encoding() {
        let old: Cursor = serde_json::from_str(
            r#"{"chat_id":42,"topic_id":null,"offset_id":7,"high_water":99,"done":false}"#,
        )
        .unwrap();
        assert_eq!(old.pages, 0);
        assert!(serde_json::to_value(&old).unwrap().get("pages").is_none());
        let current = Cursor { pages: 3, ..old };
        assert_eq!(
            serde_json::to_value(&current).unwrap()["pages"],
            serde_json::json!(3)
        );
    }
}
