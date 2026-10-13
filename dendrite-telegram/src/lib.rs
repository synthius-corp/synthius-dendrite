//! One-shot Telegram import. No cloud API, logging, persisted session, media
//! downloader or arbitrary RPC interface is provided by this crate.
pub mod journal;
pub mod network;
pub mod protocol;
pub const PROTOCOL_VERSION: u32 = 1;
