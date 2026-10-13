//! Deterministic, credential-free technical source parsing for Dendrite.
//!
//! This module deliberately knows nothing about accounts, personas, database
//! rows, prompts, providers, or pipelines. The HTTP host supplies already
//! authorized local bytes and independently scopes every input/output path.

use encoding_rs::{UTF_16BE, UTF_16LE, WINDOWS_1251, WINDOWS_1252};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

mod contract;
pub use contract::{validate_corpus_record, validate_parse_result, MAX_TEXT_CHARS};

const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CORPUS_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECORDS: usize = 250_000;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;

pub const CORPUS_SCHEMA: &str = "synthius-corpus/v1";

#[derive(Debug, Clone, Serialize)]
pub struct ParserCapability {
    pub parser_id: &'static str,
    pub extensions: &'static [&'static str],
    pub subtypes: &'static [&'static str],
}

/// Machine-readable catalogue used by the desktop host and cloud worker.
/// Adding an adapter no longer requires changing a database enum.
pub fn capabilities() -> Vec<ParserCapability> {
    vec![
        ParserCapability {
            parser_id: "telegram_json",
            extensions: &["json"],
            subtypes: &["telegram_json"],
        },
        ParserCapability {
            parser_id: "axon_stream",
            extensions: &["jsonl"],
            subtypes: &["axon_telegram", "axon_gmail"],
        },
        ParserCapability {
            parser_id: "whatsapp_text",
            extensions: &["txt"],
            subtypes: &["whatsapp_txt", "whatsapp_alt"],
        },
        ParserCapability {
            parser_id: "document_text",
            extensions: &["txt", "text"],
            subtypes: &["plain_text"],
        },
        ParserCapability {
            parser_id: "document_markdown",
            extensions: &["md", "markdown"],
            subtypes: &["markdown", "obsidian_md"],
        },
        ParserCapability {
            parser_id: "document_html",
            extensions: &["html", "htm"],
            subtypes: &["html", "apple_notes"],
        },
        ParserCapability {
            parser_id: "document_json",
            extensions: &["json"],
            subtypes: &["document_json"],
        },
        ParserCapability {
            parser_id: "document_pdf",
            extensions: &["pdf"],
            subtypes: &["pdf"],
        },
        ParserCapability {
            parser_id: "document_docx",
            extensions: &["docx"],
            subtypes: &["docx"],
        },
        ParserCapability {
            parser_id: "document_rtf",
            extensions: &["rtf"],
            subtypes: &["rtf"],
        },
        ParserCapability {
            parser_id: "document_odt",
            extensions: &["odt"],
            subtypes: &["odt"],
        },
        ParserCapability {
            parser_id: "ebook_epub",
            extensions: &["epub"],
            subtypes: &["epub"],
        },
        ParserCapability {
            parser_id: "ebook_fb2",
            extensions: &["fb2"],
            subtypes: &["fb2"],
        },
        ParserCapability {
            parser_id: "archive_text",
            extensions: &["zip"],
            subtypes: &["archive_text", "notion_export"],
        },
        ParserCapability {
            parser_id: "linkedin_export",
            extensions: &["zip"],
            subtypes: &["linkedin_export"],
        },
        ParserCapability {
            parser_id: "instagram_export",
            extensions: &["zip"],
            subtypes: &["instagram_export"],
        },
        ParserCapability {
            parser_id: "facebook_export",
            extensions: &["zip"],
            subtypes: &["facebook_export"],
        },
    ]
}

#[derive(Debug, Clone, Serialize)]
pub struct Classification {
    pub category: String,
    pub subtype: String,
    pub confidence: f64,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Participant {
    pub name: String,
    pub message_count: usize,
    pub word_count: usize,
    pub first_message: Option<String>,
    pub last_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParseResult {
    pub parser_id: String,
    pub subtype: String,
    pub parser_version: String,
    pub corpus_schema: String,
    pub corpus: Vec<u8>,
    pub corpus_sha256: String,
    pub source_sha256: String,
    pub message_count: usize,
    pub word_count: usize,
    pub participants: Vec<Participant>,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
}

#[derive(Debug, Clone)]
struct Record {
    timestamp: Option<String>,
    user: String,
    uid: String,
    text: String,
    kind: String,
    source: String,
    original_id: Option<String>,
    char_start: Option<usize>,
    char_end: Option<usize>,
    line_start: Option<usize>,
    line_end: Option<usize>,
    paragraph_start: Option<usize>,
    paragraph_end: Option<usize>,
}

fn extension(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(_, value)| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default()
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err("Source exceeds the local parser limit".to_string());
    }
    if bytes.starts_with(&[0xff, 0xfe]) {
        let (value, _, _) = UTF_16LE.decode(&bytes[2..]);
        return Ok(value.into_owned());
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        let (value, _, _) = UTF_16BE.decode(&bytes[2..]);
        return Ok(value.into_owned());
    }
    let has_nul = bytes.contains(&0);
    let forbidden_controls = bytes
        .iter()
        .filter(|byte| **byte < 0x20 && !matches!(**byte, b'\t' | b'\n' | b'\r'))
        .count();
    if has_nul || (forbidden_controls > 0 && forbidden_controls * 100 > bytes.len().max(1)) {
        return Err("Source appears to be binary rather than text".to_string());
    }
    if let Ok(value) = std::str::from_utf8(bytes) {
        return Ok(value.trim_start_matches('\u{feff}').to_string());
    }

    // Both single-byte decoders accept almost every byte. Prefer Cyrillic only
    // when it forms a substantial script-consistent body and the Western view
    // is dominated by high-byte mojibake; otherwise Windows-1252 is the safer
    // fallback for Western documents with occasional accents.
    let (cyrillic, _, _) = WINDOWS_1251.decode(bytes);
    let (western, _, _) = WINDOWS_1252.decode(bytes);
    let letter_count = cyrillic
        .chars()
        .filter(|value| value.is_alphabetic())
        .count();
    let cyrillic_count = cyrillic
        .chars()
        .filter(|value| matches!(*value, '\u{0400}'..='\u{04ff}'))
        .count();
    let western_letter_count = western
        .chars()
        .filter(|value| value.is_alphabetic())
        .count();
    let western_high_count = western
        .chars()
        .filter(|value| value.is_alphabetic() && !value.is_ascii())
        .count();
    if cyrillic_count >= 3
        && cyrillic_count * 10 >= letter_count.max(1) * 6
        && western_high_count * 10 >= western_letter_count.max(1) * 6
    {
        Ok(cyrillic.into_owned())
    } else {
        Ok(western.into_owned())
    }
}

fn normalized_plain_text(value: &str) -> String {
    let newline = Regex::new(r"(?i)<br\s*/?>|</p\s*>|</div\s*>|</li\s*>|</h[1-6]\s*>").unwrap();
    let tags = Regex::new(r"(?s)<[^>]{1,4096}>").unwrap();
    let markdown = Regex::new(r"(?m)^\s{0,3}(?:#{1,6}\s+|[-*+]\s+|>\s?)|[*_`~]{1,3}").unwrap();
    let spaces = Regex::new(r"[ \t\r]+").unwrap();
    let blank = Regex::new(r"\n{3,}").unwrap();
    let with_newlines = newline.replace_all(value, "\n");
    let without_tags = tags.replace_all(&with_newlines, " ");
    let without_markdown = markdown.replace_all(&without_tags, "");
    let without_spaces = spaces.replace_all(&without_markdown, " ");
    blank
        .replace_all(without_spaces.trim(), "\n\n")
        .into_owned()
}

fn decode_xml_text(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn xml_to_text(xml: &str) -> String {
    let boundaries =
        Regex::new(r"(?i)</?(?:w:p|text:p|text:h|p|h[1-6]|div|li|br)(?:\s[^>]*)?/?>").unwrap();
    let tags = Regex::new(r"(?s)<[^>]{1,4096}>").unwrap();
    let value = boundaries.replace_all(xml, "\n");
    normalized_plain_text(&decode_xml_text(&tags.replace_all(&value, " ")))
}

fn bounded_read(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut output)
        .map_err(|_| "Archive member could not be read".to_string())?;
    if output.len() > limit {
        return Err("Archive member exceeds the parser limit".into());
    }
    Ok(output)
}

fn bounded_zip_member(bytes: &[u8], member: &str) -> Result<String, String> {
    // Validate every member before reading one requested path. A valid DOCX,
    // ODT or EPUB containing a second traversal entry is still hostile.
    archive_member_names(bytes)?;
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Invalid archive".to_string())?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err("Archive contains too many entries".to_string());
    }
    let total = (0..archive.len()).try_fold(0_u64, |sum, index| {
        let file = archive
            .by_index(index)
            .map_err(|_| "Invalid archive entry".to_string())?;
        sum.checked_add(file.size())
            .ok_or_else(|| "Archive size overflow".to_string())
    })?;
    if total > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
        return Err("Archive expands beyond the parser limit".to_string());
    }
    let file = archive
        .by_name(member)
        .map_err(|_| "Required document content is missing".to_string())?;
    if file.size() > MAX_SOURCE_BYTES as u64 {
        return Err("Document text exceeds the parser limit".to_string());
    }
    let expected_size = file.size();
    let output = bounded_read(file, MAX_SOURCE_BYTES)?;
    if output.len() as u64 != expected_size {
        return Err("Archive member size mismatch".into());
    }
    String::from_utf8(output).map_err(|_| "Document text is not valid UTF-8".to_string())
}

fn archive_member_names(bytes: &[u8]) -> Result<Vec<String>, String> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Invalid archive".to_string())?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err("Archive contains too many entries".to_string());
    }
    let mut total = 0_u64;
    let mut names = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| "Invalid archive entry".to_string())?;
        total = total
            .checked_add(file.size())
            .ok_or_else(|| "Archive size overflow".to_string())?;
        if total > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
            return Err("Archive expands beyond the parser limit".to_string());
        }
        let name = file.name().replace('\\', "/");
        if name.starts_with('/') || name.split('/').any(|part| part == "..") {
            return Err("Archive contains an unsafe path".to_string());
        }
        names.push(name);
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    Ok(names)
}

fn bounded_archive_text(bytes: &[u8]) -> Result<String, String> {
    let names = archive_member_names(bytes)?;
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Invalid archive".to_string())?;
    let mut output = String::new();
    let mut actual_expanded = 0_u64;
    for name in names {
        let lower = name.to_ascii_lowercase();
        if !matches!(
            extension(&lower).as_str(),
            ".txt" | ".md" | ".markdown" | ".csv" | ".json" | ".html" | ".htm" | ".xml" | ".xhtml"
        ) {
            continue;
        }
        let file = archive
            .by_name(&name)
            .map_err(|_| "Invalid archive entry".to_string())?;
        if file.is_dir() {
            continue;
        }
        if file.size() > MAX_SOURCE_BYTES as u64 {
            return Err("Archive member exceeds the parser limit".into());
        }
        let expected_size = file.size();
        let member = bounded_read(file, MAX_SOURCE_BYTES)?;
        if member.len() as u64 != expected_size {
            return Err("Archive member size mismatch".into());
        }
        actual_expanded += member.len() as u64;
        if actual_expanded > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
            return Err("Archive expands beyond the parser limit".into());
        }
        let decoded = decode_text(&member)?;
        let normalized = if matches!(
            extension(&lower).as_str(),
            ".html" | ".htm" | ".xml" | ".xhtml"
        ) {
            xml_to_text(&decoded)
        } else {
            normalized_plain_text(&decoded)
        };
        if normalized.is_empty() {
            continue;
        }
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&normalized);
        if output.len() > MAX_CORPUS_BYTES {
            return Err("Archive text exceeds the parser limit".to_string());
        }
    }
    if output.is_empty() {
        return Err("Archive contains no supported textual source".to_string());
    }
    Ok(output)
}

fn rtf_to_text(value: &str) -> String {
    let hex = Regex::new(r"\\'[0-9a-fA-F]{2}").unwrap();
    let controls = Regex::new(r"\\[a-zA-Z]+-?\d* ?|\\[^a-zA-Z0-9]").unwrap();
    let braces = Regex::new(r"[{}]").unwrap();
    let value = value.replace("\\par", "\n").replace("\\line", "\n");
    let value = hex.replace_all(&value, " ");
    let value = controls.replace_all(&value, " ");
    normalized_plain_text(&braces.replace_all(&value, " "))
}

fn extract_document_text(filename: &str, bytes: &[u8]) -> Result<(String, &'static str), String> {
    let ext = extension(filename);
    match ext.as_str() {
        ".json" | ".txt" | ".text" | ".md" | ".markdown" | ".html" | ".htm" => {
            let text = decode_text(bytes)?;
            Ok((
                normalized_plain_text(&text),
                match ext.as_str() {
                    ".json" => "document_json",
                    ".md" | ".markdown" => "document_markdown",
                    ".html" | ".htm" => "document_html",
                    _ => "document_text",
                },
            ))
        }
        ".rtf" => Ok((rtf_to_text(&decode_text(bytes)?), "document_rtf")),
        ".docx" => Ok((
            xml_to_text(&bounded_zip_member(bytes, "word/document.xml")?),
            "document_docx",
        )),
        ".odt" => Ok((
            xml_to_text(&bounded_zip_member(bytes, "content.xml")?),
            "document_odt",
        )),
        ".epub" => Ok((bounded_archive_text(bytes)?, "ebook_epub")),
        ".fb2" => Ok((xml_to_text(&decode_text(bytes)?), "ebook_fb2")),
        ".zip" => Ok((bounded_archive_text(bytes)?, "archive_text")),
        ".pdf" => {
            let text = pdf_extract::extract_text_from_mem(bytes)
                .map_err(|_| "PDF text extraction failed".to_string())?;
            Ok((normalized_plain_text(&text), "document_pdf"))
        }
        _ => Err("Unsupported local parser format".to_string()),
    }
}

fn flatten_telegram_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(flatten_telegram_text)
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(map) => map
            .get("text")
            .map(flatten_telegram_text)
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn telegram_messages(value: &Value) -> Vec<&Value> {
    if let Some(messages) = value.get("messages").and_then(Value::as_array) {
        return messages.iter().collect();
    }
    let chats = value
        .get("chats")
        .and_then(|item| item.get("list").or_else(|| item.as_array().map(|_| item)))
        .and_then(Value::as_array);
    chats
        .into_iter()
        .flat_map(|items| items.iter())
        .flat_map(|chat| {
            chat.get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .collect()
}

fn is_telegram_message(message: &Value) -> bool {
    let sender = message.get("from").is_some()
        || message.get("from_id").is_some()
        || message.get("actor").is_some()
        || message.get("actor_id").is_some();
    let has_text = message.get("text").is_some() || message.get("caption").is_some();
    let timestamp = message.get("date").is_some() || message.get("date_unixtime").is_some();
    // Service rows can contain actors, timestamps and labels but are not
    // user-authored messages and the parser intentionally does not emit them.
    // Detection must therefore require the same row kind that parsing accepts.
    let recognized_type = message.get("type").and_then(Value::as_str) == Some("message");
    sender && has_text && timestamp && recognized_type
}

fn parse_telegram(bytes: &[u8]) -> Result<Vec<Record>, String> {
    let data: Value =
        serde_json::from_slice(bytes).map_err(|_| "Invalid JSON source".to_string())?;
    let messages = telegram_messages(&data);
    if messages.is_empty() {
        return Err("JSON source is not a supported Telegram export".to_string());
    }
    let mut records = Vec::new();
    for message in messages {
        if message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message")
            != "message"
        {
            continue;
        }
        let text_value = message
            .get("text")
            .or_else(|| message.get("caption"))
            .unwrap_or(&Value::Null);
        let text = flatten_telegram_text(text_value).trim().to_string();
        if text.is_empty() {
            continue;
        }
        let sender = message.get("sender").and_then(Value::as_object);
        let user = message
            .get("from")
            .or_else(|| message.get("from_name"))
            .or_else(|| sender.and_then(|value| value.get("name")))
            .and_then(Value::as_str)
            .unwrap_or("Unknown")
            .trim()
            .to_string();
        let uid = message
            .get("from_id")
            .or_else(|| sender.and_then(|value| value.get("id")))
            .and_then(Value::as_str)
            .unwrap_or(&user)
            .to_string();
        records.push(Record {
            timestamp: message
                .get("date")
                .or_else(|| message.get("date_unixtime"))
                .and_then(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| value.as_i64().map(|number| number.to_string()))
                }),
            user,
            uid,
            text,
            kind: "message".to_string(),
            source: "telegram".to_string(),
            original_id: message
                .get("id")
                .map(|value| value.to_string().trim_matches('"').to_string()),
            char_start: None,
            char_end: None,
            line_start: None,
            line_end: None,
            paragraph_start: None,
            paragraph_end: None,
        });
    }
    Ok(records)
}

fn axon_stream_provider(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let first = text.lines().find(|line| !line.trim().is_empty())?;
    let value: Value = serde_json::from_str(first).ok()?;
    if value.get("schema").and_then(Value::as_str) != Some("synthius-axon/v1") {
        return None;
    }
    match value.get("provider").and_then(Value::as_str) {
        Some("telegram") => Some("telegram".to_string()),
        Some("gmail") => Some("gmail".to_string()),
        _ => None,
    }
}

fn parse_axon(bytes: &[u8], expected_provider: &str) -> Result<Vec<Record>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "Axon stream is not UTF-8".to_string())?;
    let mut records = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|_| format!("Invalid Axon record at line {}", line_index + 1))?;
        if value.get("schema").and_then(Value::as_str) != Some("synthius-axon/v1")
            || value.get("provider").and_then(Value::as_str) != Some(expected_provider)
        {
            return Err(format!(
                "Mixed or unsupported Axon record at line {}",
                line_index + 1
            ));
        }
        let body = value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if body.is_empty() {
            continue;
        }
        let author = value
            .get("author")
            .and_then(Value::as_str)
            .filter(|entry| !entry.trim().is_empty())
            .unwrap_or("Unknown")
            .trim()
            .to_string();
        let original_id = value
            .get("external_id")
            .and_then(Value::as_str)
            .filter(|entry| !entry.trim().is_empty())
            .map(str::to_string);
        let revision = value
            .get("revision")
            .and_then(Value::as_str)
            .unwrap_or("original");
        records.push(Record {
            timestamp: value
                .get("event_at")
                .and_then(Value::as_str)
                .map(str::to_string),
            user: author.clone(),
            uid: author,
            text: body.to_string(),
            kind: value
                .get("event_kind")
                .and_then(Value::as_str)
                .unwrap_or("message")
                .to_string(),
            source: format!("axon_{}", expected_provider),
            original_id: original_id.map(|id| format!("{}:{}", id, revision)),
            char_start: None,
            char_end: None,
            line_start: Some(line_index + 1),
            line_end: Some(line_index + 1),
            paragraph_start: None,
            paragraph_end: None,
        });
    }
    if records.is_empty() {
        return Err("Axon stream contains no readable records".to_string());
    }
    Ok(records)
}

fn parse_whatsapp(text: &str) -> Result<Vec<Record>, String> {
    let patterns = [
        Regex::new(r"^(\d{1,2}\.\d{1,2}\.\d{2,4})\s+(\d{1,2}:\d{2})\s*[-–]\s*([^-]{1,200}?)\s*[-–]\s*(.*)$").unwrap(),
        Regex::new(r"^\[?(\d{1,2}[./]\d{1,2}[./]\d{2,4}),?\s*(\d{1,2}:\d{2}(?::\d{2})?)\]?\s*([^:]{1,200}?):\s*(.*)$").unwrap(),
        Regex::new(r"^(\d{1,2}[./]\d{1,2}[./]\d{2,4}),?\s*(\d{1,2}:\d{2})\s*[-–]\s*([^:]{1,200}?):\s*(.*)$").unwrap(),
    ];
    let mut records: Vec<Record> = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let mut matched = false;
        for pattern in &patterns {
            if let Some(capture) = pattern.captures(line.trim_end()) {
                let body = capture.get(4).map(|m| m.as_str().trim()).unwrap_or("");
                if !body.is_empty() {
                    let user = capture
                        .get(3)
                        .map(|m| m.as_str().trim())
                        .unwrap_or("Unknown")
                        .to_string();
                    records.push(Record {
                        timestamp: Some(format!("{} {}", &capture[1], &capture[2])),
                        uid: user.clone(),
                        user,
                        text: body.to_string(),
                        kind: "message".to_string(),
                        source: "whatsapp".to_string(),
                        original_id: None,
                        char_start: None,
                        char_end: None,
                        line_start: Some(line_number),
                        line_end: Some(line_number),
                        paragraph_start: None,
                        paragraph_end: None,
                    });
                }
                matched = true;
                break;
            }
        }
        if !matched && !line.trim().is_empty() {
            if let Some(previous) = records.last_mut() {
                previous.text.push('\n');
                previous.text.push_str(line.trim());
                previous.line_end = Some(line_number);
            }
        }
    }
    if records.is_empty() {
        return Err("Text source is not a supported WhatsApp export".to_string());
    }
    Ok(records)
}

fn whatsapp_subtype(text: &str) -> Option<&'static str> {
    let standard = Regex::new(
        r"^(\d{1,2}\.\d{1,2}\.\d{2,4})\s+(\d{1,2}:\d{2})\s*[-–]\s*([^-]{1,200}?)\s*[-–]\s*(.+)$",
    )
    .unwrap();
    let bracket = Regex::new(
        r"^\[?(\d{1,2}[./]\d{1,2}[./]\d{2,4}),?\s*(\d{1,2}:\d{2}(?::\d{2})?)\]?\s*([^:]{1,200}?):\s*(.+)$",
    )
    .unwrap();
    let comma = Regex::new(
        r"^(\d{1,2}[./]\d{1,2}[./]\d{2,4}),?\s*(\d{1,2}:\d{2})\s*[-–]\s*([^:]{1,200}?):\s*(.+)$",
    )
    .unwrap();
    let mut standard_count = 0_usize;
    let mut alternate_count = 0_usize;
    for line in text
        .lines()
        .take(500)
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if standard.is_match(line) {
            standard_count += 1;
        } else if bracket.is_match(line) || comma.is_match(line) {
            alternate_count += 1;
        }
    }
    match (standard_count, alternate_count) {
        (0, 0) => None,
        (standard, alternate) if standard >= alternate => Some("whatsapp_txt"),
        _ => Some("whatsapp_alt"),
    }
}

fn document_records(text: &str, subject: &str, category: &str) -> Vec<Record> {
    let mut records = Vec::new();
    let mut pending = String::new();
    let mut pending_start = 1_usize;
    let mut pending_end = 1_usize;
    for (paragraph_index, block) in text.split("\n\n").enumerate() {
        let paragraph_number = paragraph_index + 1;
        let block = block.trim();
        if block.is_empty() {
            continue;
        }
        if !pending.is_empty() {
            pending.push(' ');
        } else {
            pending_start = paragraph_number;
        }
        pending.push_str(block);
        pending_end = paragraph_number;
        if pending.split_whitespace().count() < 30 {
            continue;
        }
        let normalized = pending.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() {
            records.push(Record {
                timestamp: None,
                user: subject.to_string(),
                uid: "document".to_string(),
                text: normalized,
                kind: category.to_string(),
                source: "document".to_string(),
                original_id: None,
                char_start: None,
                char_end: None,
                line_start: None,
                line_end: None,
                paragraph_start: Some(pending_start),
                paragraph_end: Some(pending_end),
            });
        }
        pending.clear();
    }
    if !pending.trim().is_empty() {
        records.push(Record {
            timestamp: None,
            user: subject.to_string(),
            uid: "document".to_string(),
            text: pending.trim().to_string(),
            kind: category.to_string(),
            source: "document".to_string(),
            original_id: None,
            char_start: None,
            char_end: None,
            line_start: None,
            line_end: None,
            paragraph_start: Some(pending_start),
            paragraph_end: Some(pending_end),
        });
    }
    records
}

fn document_subtype(ext: &str) -> &'static str {
    match ext {
        ".md" | ".markdown" => "markdown",
        ".html" | ".htm" => "html",
        ".pdf" => "pdf",
        ".docx" => "docx",
        ".rtf" => "rtf",
        ".odt" => "odt",
        ".epub" => "epub",
        ".fb2" => "fb2",
        ".json" => "document_json",
        _ => "plain_text",
    }
}

fn keyword_score(haystack: &str, words: &[&str]) -> usize {
    words
        .iter()
        .filter(|word| haystack.contains(**word))
        .count()
}

fn hints_for_text(filename: &str, text: &str) -> Classification {
    let lower_name = filename.to_lowercase().replace(['_', '-', '.'], " ");
    let lower = text
        .chars()
        .take(200_000)
        .collect::<String>()
        .to_lowercase();
    let ext = extension(filename);
    if ext == ".txt" && parse_whatsapp(text).is_ok() {
        return Classification {
            category: "chat".into(),
            subtype: whatsapp_subtype(text).unwrap_or("whatsapp_txt").into(),
            confidence: 0.9,
            hints: vec!["WhatsApp message structure".into()],
        };
    }
    if matches!(ext.as_str(), ".md" | ".markdown") {
        let frontmatter =
            text.trim_start().starts_with("---\n") || text.trim_start().starts_with("---\r\n");
        let obsidian_link = text.contains("[[") && text.contains("]]");
        if frontmatter && obsidian_link {
            return Classification {
                category: "notes".into(),
                subtype: "obsidian_md".into(),
                confidence: 0.92,
                hints: vec!["Obsidian frontmatter and wikilink structure".into()],
            };
        }
    }
    let groups: [(&str, &[&str]); 6] = [
        (
            "therapy",
            &[
                "therapy",
                "therapist",
                "counseling",
                "counsellor",
                "психолог",
                "терапевт",
                "сессия",
                "тревога",
                "coping",
            ],
        ),
        (
            "interview",
            &[
                "self interview",
                "interview transcript",
                "interviewer:",
                "question:",
                "интервью",
                "вопрос:",
            ],
        ),
        (
            "cv",
            &[
                " cv ",
                "resume",
                "curriculum vitae",
                "work experience",
                "employment history",
                "education",
                "linkedin profile",
                "резюме",
                "опыт работы",
            ],
        ),
        (
            "biography",
            &[
                "biography",
                "master bio",
                "memoir",
                "life story",
                "autobiography",
                "биография",
                "мемуар",
            ],
        ),
        (
            "description",
            &[
                "personality",
                "character traits",
                "personal profile",
                "личность",
                "характер",
            ],
        ),
        (
            "dialogs",
            &[
                "letter",
                "correspondence",
                "email thread",
                "письмо",
                "переписка",
            ],
        ),
    ];
    let mut best: Option<(&str, usize, bool)> = None;
    for (category, words) in groups {
        let padded_name = format!(" {lower_name} ");
        let name_match = words.iter().any(|word| padded_name.contains(word));
        let score = keyword_score(&lower, words) + usize::from(name_match) * 2;
        if score > best.map(|(_, best_score, _)| best_score).unwrap_or(0) {
            best = Some((category, score, name_match));
        }
    }
    if let Some((category, score, name_match)) = best.filter(|(_, score, _)| *score >= 2) {
        return Classification {
            category: category.into(),
            subtype: document_subtype(&ext).into(),
            confidence: if score >= 4 {
                0.82
            } else if name_match {
                0.72
            } else {
                0.64
            },
            hints: vec!["Bounded filename and content evidence".into()],
        };
    }
    Classification {
        category: "notes".into(),
        subtype: document_subtype(&ext).into(),
        confidence: 0.5,
        hints: vec!["General document".into()],
    }
}

pub fn classify(filename: &str, bytes: &[u8]) -> Result<Classification, String> {
    if bytes.is_empty() || bytes.len() > MAX_SOURCE_BYTES {
        return Err("Source is empty or exceeds the local parser limit".to_string());
    }
    let ext = extension(filename);
    if ext == ".jsonl" {
        if let Some(provider) = axon_stream_provider(bytes) {
            return Ok(Classification {
                category: if provider == "telegram" {
                    "chat"
                } else {
                    "dialogs"
                }
                .into(),
                subtype: format!("axon_{}", provider),
                confidence: 1.0,
                hints: vec!["Synthius Axon stream envelope".into()],
            });
        }
    }
    if bytes.starts_with(b"%PDF-") {
        let sample = extract_document_text("source.pdf", bytes)?.0;
        let mut result = hints_for_text(filename, &sample);
        result.subtype = "pdf".into();
        result.hints.insert(0, "PDF magic signature".into());
        return Ok(result);
    }
    if bytes.starts_with(b"{\\rtf") {
        let mut result = hints_for_text(filename, &rtf_to_text(&decode_text(bytes)?));
        result.subtype = "rtf".into();
        result.hints.insert(0, "RTF magic signature".into());
        return Ok(result);
    }
    if bytes.starts_with(b"PK\x03\x04") {
        let names = archive_member_names(bytes)?;
        let names_lower = names
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<Vec<_>>();
        let exact = |needle: &str| names_lower.iter().any(|name| name == needle);
        let contains = |needle: &str| names_lower.iter().any(|name| name.contains(needle));
        let has_basename = |needle: &str| {
            names_lower
                .iter()
                .any(|name| name.rsplit('/').next() == Some(needle))
        };
        let notion_page_name =
            Regex::new(r"(?i)(?:^|/)[^/]*[ _-][0-9a-f]{32}\.(?:md|markdown|html|htm)$").unwrap();
        let notion_pages = names_lower
            .iter()
            .filter(|name| notion_page_name.is_match(name))
            .count();
        let classification =
            if exact("word/document.xml") && exact("[content_types].xml") {
                let sample = extract_document_text("source.docx", bytes)?.0;
                let mut result = hints_for_text(filename, &sample);
                result.subtype = "docx".into();
                result.hints.insert(0, "DOCX container structure".into());
                result
            } else if exact("meta-inf/container.xml") {
                Classification {
                    category: "biography".into(),
                    subtype: "epub".into(),
                    confidence: 0.98,
                    hints: vec!["EPUB container structure".into()],
                }
            } else if exact("content.xml") && exact("meta-inf/manifest.xml") {
                let sample = extract_document_text("source.odt", bytes)?.0;
                let mut result = hints_for_text(filename, &sample);
                result.subtype = "odt".into();
                result.hints.insert(0, "ODT container structure".into());
                result
            } else if (has_basename("profile.csv") && has_basename("positions.csv"))
                || (has_basename("profile.csv") && has_basename("connections.csv"))
            {
                Classification {
                    category: "cv".into(),
                    subtype: "linkedin_export".into(),
                    confidence: 0.95,
                    hints: vec!["LinkedIn export structure".into()],
                }
            } else if contains("personal_information/personal_information.json")
                || (has_basename("personal_information.json")
                    && (contains("content/posts")
                        || names_lower.iter().any(|name| {
                            name.rsplit('/')
                                .next()
                                .is_some_and(|base| base.starts_with("posts_"))
                        })))
            {
                Classification {
                    category: "notes".into(),
                    subtype: "instagram_export".into(),
                    confidence: 0.94,
                    hints: vec!["Instagram export structure".into()],
                }
            } else if names_lower.iter().any(|name| {
                name.starts_with("messages/inbox/") || name.contains("/messages/inbox/")
            }) || (names_lower.iter().any(|name| {
                name.starts_with("profile_information/") || name.contains("/profile_information/")
            }) && names_lower.iter().any(|name| {
                name.starts_with("posts/")
                    || name.contains("/posts/")
                    || name
                        .rsplit('/')
                        .next()
                        .is_some_and(|base| base.starts_with("your_posts"))
            })) {
                Classification {
                    category: "notes".into(),
                    subtype: "facebook_export".into(),
                    confidence: 0.94,
                    hints: vec!["Facebook export structure".into()],
                }
            } else if notion_pages >= 2 {
                Classification {
                    category: "notes".into(),
                    subtype: "notion_export".into(),
                    confidence: 0.9,
                    hints: vec!["Notion export page identifier structure".into()],
                }
            } else if names_lower.iter().any(|name| {
                matches!(
                    extension(name).as_str(),
                    ".txt"
                        | ".md"
                        | ".markdown"
                        | ".csv"
                        | ".json"
                        | ".html"
                        | ".htm"
                        | ".xml"
                        | ".xhtml"
                )
            }) {
                Classification {
                    category: "notes".into(),
                    subtype: "archive_text".into(),
                    confidence: 0.75,
                    hints: vec!["Bounded textual archive structure".into()],
                }
            } else {
                return Err("Unsupported archive structure".to_string());
            };
        return Ok(classification);
    }
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) == Some(&b'{') {
        match serde_json::from_slice::<Value>(bytes) {
            Ok(json)
                if telegram_messages(&json)
                    .iter()
                    .any(|message| is_telegram_message(message)) =>
            {
                return Ok(Classification {
                    category: "chat".into(),
                    subtype: "telegram_json".into(),
                    confidence: 0.95,
                    hints: vec!["Telegram export structure".into()],
                });
            }
            Err(_) if ext == ".json" => return Err("Invalid JSON source".to_string()),
            _ => {}
        }
    }
    // Textual container signatures are stronger than a misleading extension.
    // Keep this bounded: only the prefix is inspected before the full parser is
    // selected, and the ordinary source-size limit has already been applied.
    let textual_probe = if bytes.starts_with(b"<")
        || bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || matches!(ext.as_str(), ".html" | ".htm" | ".fb2")
    {
        Some(decode_text(bytes)?)
    } else {
        None
    };
    if let Some(probe) = textual_probe {
        let lower = probe
            .trim_start_matches('\u{feff}')
            .trim_start()
            .chars()
            .take(4096)
            .collect::<String>()
            .to_ascii_lowercase();
        if lower.starts_with("<!doctype html")
            || lower.starts_with("<html")
            || lower.contains("<html ")
        {
            let mut result = hints_for_text(filename, &normalized_plain_text(&probe));
            let apple_notes =
                lower.contains("name=\"generator\"") && lower.contains("content=\"apple notes\"");
            result.subtype = if apple_notes { "apple_notes" } else { "html" }.into();
            result.hints.insert(
                0,
                if apple_notes {
                    "Apple Notes HTML export structure".into()
                } else {
                    "HTML document structure".into()
                },
            );
            return Ok(result);
        }
        if lower.starts_with("<fictionbook")
            || (lower.starts_with("<?xml") && lower.contains("<fictionbook"))
        {
            let mut result = hints_for_text(filename, &xml_to_text(&probe));
            if result.category == "notes" {
                result.category = "biography".into();
                result.confidence = 0.9;
            }
            result.subtype = "fb2".into();
            result.hints.insert(0, "FB2 XML structure".into());
            return Ok(result);
        }
    }
    if !matches!(
        ext.as_str(),
        ".json"
            | ".txt"
            | ".text"
            | ".md"
            | ".markdown"
            | ".html"
            | ".htm"
            | ".pdf"
            | ".docx"
            | ".rtf"
            | ".odt"
            | ".epub"
            | ".fb2"
            | ".zip"
            | ".jsonl"
    ) {
        return Err("Unsupported local parser format".to_string());
    }
    let sample = if ext == ".json"
        || matches!(
            ext.as_str(),
            ".txt" | ".text" | ".md" | ".markdown" | ".html" | ".htm" | ".rtf" | ".fb2"
        ) {
        decode_text(bytes)?
    } else {
        extract_document_text(filename, bytes)?.0
    };
    let mut result = hints_for_text(filename, &sample);
    if ext == ".fb2" && result.category == "notes" {
        result.category = "biography".into();
        result.confidence = 0.7;
        result.hints.insert(0, "FB2 ebook extension".into());
    }
    Ok(result)
}

pub fn parse(
    filename: &str,
    bytes: &[u8],
    category: &str,
    _subtype: &str,
    subject_name: &str,
) -> Result<ParseResult, String> {
    if bytes.is_empty() || bytes.len() > MAX_SOURCE_BYTES {
        return Err("Source is empty or exceeds the local parser limit".to_string());
    }
    let source_sha256 = format!("{:x}", Sha256::digest(bytes));
    let classification = classify(filename, bytes)?;
    let detected_subtype = classification.subtype;
    let (records, parser_id) = if detected_subtype == "axon_telegram" {
        (parse_axon(bytes, "telegram")?, "axon_stream")
    } else if detected_subtype == "axon_gmail" {
        (parse_axon(bytes, "gmail")?, "axon_stream")
    } else if category == "chat" && detected_subtype == "telegram_json" {
        (parse_telegram(bytes)?, "telegram_json")
    } else if category == "chat"
        && matches!(detected_subtype.as_str(), "whatsapp_txt" | "whatsapp_alt")
    {
        (parse_whatsapp(&decode_text(bytes)?)?, "whatsapp_text")
    } else {
        let detected_filename = match detected_subtype.as_str() {
            "pdf" => "source.pdf",
            "docx" => "source.docx",
            "rtf" => "source.rtf",
            "odt" => "source.odt",
            "epub" => "source.epub",
            "fb2" => "source.fb2",
            "html" | "apple_notes" => "source.html",
            "archive_text" | "notion_export" | "linkedin_export" | "instagram_export"
            | "facebook_export" => "source.zip",
            _ => filename,
        };
        let (text, generic_parser_id) = extract_document_text(detected_filename, bytes)?;
        let parser_id = match detected_subtype.as_str() {
            "archive_text" | "notion_export" => "archive_text",
            "linkedin_export" => "linkedin_export",
            "instagram_export" => "instagram_export",
            "facebook_export" => "facebook_export",
            _ => generic_parser_id,
        };
        let subject = if subject_name.trim().is_empty() {
            "User"
        } else {
            subject_name.trim()
        };
        (document_records(&text, subject, category), parser_id)
    };
    let mut normalized = Vec::new();
    for mut record in records {
        record.user = match record.user.trim() {
            "" => "Unknown".to_string(),
            name => name.to_string(),
        };
        let original_text = std::mem::take(&mut record.text);
        let text = original_text.trim();
        if text.is_empty() {
            continue;
        }
        let char_count = text.chars().count();
        if char_count <= MAX_TEXT_CHARS {
            record.text = text.to_string();
            normalized.push(record);
        } else {
            // Preserve the complete normalized message instead of rejecting or
            // truncating a large history row. Offsets use Unicode scalar values
            // in this normalized message; line/paragraph references retain the
            // original enclosing source range.
            let mut rest = text;
            let mut offset = 0;
            while !rest.is_empty() {
                let boundary = rest
                    .char_indices()
                    .nth(MAX_TEXT_CHARS)
                    .map_or(rest.len(), |(i, _)| i);
                let raw = &rest[..boundary];
                let chunk = raw.trim();
                if !chunk.is_empty() {
                    let leading = raw.chars().take_while(|c| c.is_whitespace()).count();
                    let mut part = record.clone();
                    part.text = chunk.into();
                    part.char_start = Some(offset + leading);
                    part.char_end = Some(offset + leading + chunk.chars().count());
                    normalized.push(part);
                }
                if normalized.len() > MAX_RECORDS {
                    return Err("Parser exceeded the record limit".into());
                }
                offset += raw.chars().count();
                rest = &rest[boundary..];
            }
        }
        if normalized.len() > MAX_RECORDS {
            return Err("Parser exceeded the record limit".into());
        }
    }
    let records = normalized;
    if records.is_empty() || records.len() > MAX_RECORDS {
        return Err("Parser produced no records or exceeded the record limit".to_string());
    }

    let mut participant_stats: BTreeMap<String, (usize, usize, Option<String>, Option<String>)> =
        BTreeMap::new();
    let mut corpus = Vec::new();
    let mut word_count = 0_usize;
    let id_prefix = &source_sha256[..16];
    let mut date_start: Option<String> = None;
    let mut date_end: Option<String> = None;
    for (index, record) in records.iter().enumerate() {
        let text = record.text.trim();
        if text.is_empty() {
            continue;
        }
        let words = text.split_whitespace().count();
        word_count += words;
        let stats = participant_stats
            .entry(record.user.clone())
            .or_insert((0, 0, None, None));
        stats.0 += 1;
        stats.1 += words;
        if let Some(timestamp) = &record.timestamp {
            stats.2 = Some(
                stats
                    .2
                    .as_ref()
                    .map_or(timestamp.clone(), |value| value.min(timestamp).clone()),
            );
            stats.3 = Some(
                stats
                    .3
                    .as_ref()
                    .map_or(timestamp.clone(), |value| value.max(timestamp).clone()),
            );
            date_start = Some(
                date_start
                    .as_ref()
                    .map_or(timestamp.clone(), |value| value.min(timestamp).clone()),
            );
            date_end = Some(
                date_end
                    .as_ref()
                    .map_or(timestamp.clone(), |value| value.max(timestamp).clone()),
            );
        }
        let mut reference_metadata = serde_json::Map::new();
        reference_metadata.insert("version".to_string(), json!(1));
        reference_metadata.insert("record".to_string(), json!(index + 1));
        reference_metadata.insert(
            "text_sha256".to_string(),
            json!(format!("{:x}", Sha256::digest(text.as_bytes()))),
        );
        if let Some(value) = &record.original_id {
            reference_metadata.insert("original_id".to_string(), json!(value));
        }
        if let Some(value) = record.char_start {
            reference_metadata.insert("char_start".into(), json!(value));
        }
        if let Some(value) = record.char_end {
            reference_metadata.insert("char_end".into(), json!(value));
        }
        if let Some(value) = record.line_start {
            reference_metadata.insert("line_start".to_string(), json!(value));
        }
        if let Some(value) = record.line_end {
            reference_metadata.insert("line_end".to_string(), json!(value));
        }
        if let Some(value) = record.paragraph_start {
            reference_metadata.insert("paragraph_start".to_string(), json!(value));
        }
        if let Some(value) = record.paragraph_end {
            reference_metadata.insert("paragraph_end".to_string(), json!(value));
        }
        let value = json!({
            "id": format!("d_{}_{}", id_prefix, index),
            "ts": record.timestamp,
            "user": record.user,
            "uid": record.uid,
            "text": text,
            "type": record.kind,
            "source": record.source,
            "reference_metadata": reference_metadata,
        });
        validate_corpus_record(&value, index + 1)?;
        let encoded =
            serde_json::to_vec(&value).map_err(|_| "Corpus serialization failed".to_string())?;
        corpus.extend_from_slice(&encoded);
        corpus.push(b'\n');
        if corpus.len() > MAX_CORPUS_BYTES {
            return Err("Corpus exceeds the local parser limit".to_string());
        }
    }
    let message_count = corpus.iter().filter(|byte| **byte == b'\n').count();
    let participants = participant_stats
        .into_iter()
        .map(|(name, (messages, words, first, last))| Participant {
            name,
            message_count: messages,
            word_count: words,
            first_message: first,
            last_message: last,
        })
        .collect();
    let corpus_sha256 = format!("{:x}", Sha256::digest(&corpus));
    Ok(ParseResult {
        parser_id: parser_id.to_string(),
        subtype: detected_subtype,
        parser_version: env!("CARGO_PKG_VERSION").to_string(),
        corpus_schema: CORPUS_SCHEMA.to_string(),
        corpus,
        corpus_sha256,
        source_sha256,
        message_count,
        word_count,
        participants,
        date_start,
        date_end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_documents_deterministically_without_private_ids() {
        let bytes = b"A concrete synthetic paragraph about work, learning, family, routines, and goals. It has enough words to be useful for deterministic parsing without containing private application identifiers or credentials.";
        let first = parse("notes.txt", bytes, "notes", "plain_text", "Synthetic User").unwrap();
        let second = parse("notes.txt", bytes, "notes", "plain_text", "Synthetic User").unwrap();
        assert_eq!(first.corpus, second.corpus);
        assert_eq!(first.corpus_sha256, second.corpus_sha256);
        let text = String::from_utf8(first.corpus).unwrap();
        assert!(!text.contains("persona_id"));
        assert!(!text.contains("user_id"));
    }

    #[test]
    fn text_decoding_distinguishes_cyrillic_western_and_binary_sources() {
        let cyrillic =
            decode_text(&[0xcf, 0xf0, 0xe8, 0xe2, 0xe5, 0xf2, 0x20, 0xec, 0xe8, 0xf0]).unwrap();
        assert_eq!(cyrillic, "Привет мир");

        let western = decode_text(b"Caf\xe9 d\xe9j\xe0 vu").unwrap();
        assert_eq!(western, "Café déjà vu");
        assert!(decode_text(b"text\0with\0binary").is_err());
    }

    #[test]
    fn published_corpus_schema_matches_emitted_record_contract() {
        let schema: Value =
            serde_json::from_str(include_str!("../../schemas/synthius-corpus-v1.schema.json"))
                .unwrap();
        assert_eq!(
            schema.get("$id").and_then(Value::as_str),
            Some("https://synthius.ai/schemas/synthius-corpus-v1.schema.json")
        );
        let required = schema
            .get("required")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        for field in [
            "id",
            "ts",
            "user",
            "uid",
            "text",
            "type",
            "source",
            "reference_metadata",
        ] {
            assert!(required.contains(&field));
        }

        let parsed = parse(
            "notes.txt",
            b"A synthetic record with enough ordinary words to verify the published public corpus schema contract.",
            "notes",
            "plain_text",
            "Synthetic User",
        )
        .unwrap();
        let record: Value =
            serde_json::from_slice(parsed.corpus.split(|byte| *byte == b'\n').next().unwrap())
                .unwrap();
        let properties = schema.get("properties").and_then(Value::as_object).unwrap();
        for key in record.as_object().unwrap().keys() {
            assert!(properties.contains_key(key));
        }
    }

    #[test]
    fn recognizes_and_parses_telegram_json() {
        let bytes = br#"{"name":"Test","messages":[{"id":1,"type":"message","date":"2026-01-01T10:00:00","from":"Alex","from_id":"user1","text":"Hello there"}]}"#;
        let classification = classify("result.json", bytes).unwrap();
        assert_eq!(classification.subtype, "telegram_json");
        let parsed = parse("result.json", bytes, "chat", "telegram_json", "").unwrap();
        assert_eq!(parsed.message_count, 1);
        assert!(String::from_utf8(parsed.corpus)
            .unwrap()
            .contains("Hello there"));
    }

    #[test]
    fn telegram_caption_is_parsed_and_service_only_export_is_not_accepted() {
        let caption = br#"{"messages":[{"id":1,"type":"message","date_unixtime":"1767225600","from":"Alex","from_id":"user1","caption":"A captioned memory"}]}"#;
        assert_eq!(
            classify("result.json", caption).unwrap().subtype,
            "telegram_json"
        );
        let parsed = parse("result.json", caption, "chat", "telegram_json", "").unwrap();
        assert!(String::from_utf8(parsed.corpus)
            .unwrap()
            .contains("A captioned memory"));

        let service = br#"{"messages":[{"id":1,"type":"service","date":"2026-01-01","actor":"Alex","text":"created the group"}]}"#;
        assert_ne!(
            classify("result.json", service).unwrap().subtype,
            "telegram_json"
        );
    }

    #[test]
    fn generic_json_with_messages_key_is_not_misclassified_as_telegram() {
        let bytes = br#"{"messages":[{"note":"A synthetic journal entry about learning to cook."}],"context":"Personal notes"}"#;
        let classification = classify("notes.json", bytes).unwrap();
        assert_ne!(classification.subtype, "telegram_json");
        let parsed = parse(
            "notes.json",
            bytes,
            &classification.category,
            &classification.subtype,
            "Synthetic User",
        )
        .unwrap();
        assert!(String::from_utf8(parsed.corpus)
            .unwrap()
            .contains("learning to cook"));
    }

    #[test]
    fn rejects_archive_expansion_beyond_limits() {
        let mut output = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut output);
            archive
                .start_file("content.xml", zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(b"<p>Synthetic notes</p>").unwrap();
            archive.finish().unwrap();
        }
        let mut bytes = output.into_inner();
        let central = bytes
            .windows(4)
            .position(|window| window == b"PK\x01\x02")
            .unwrap();
        // A real ZIP header claiming excessive expansion must be rejected
        // before decompression. This needs no large allocation or bomb fixture.
        bytes[central + 24..central + 28]
            .copy_from_slice(&((MAX_ARCHIVE_UNCOMPRESSED_BYTES + 1) as u32).to_le_bytes());
        assert!(bounded_zip_member(&bytes, "content.xml")
            .unwrap_err()
            .contains("expands beyond"));
        assert!(classify("export.zip", &bytes)
            .unwrap_err()
            .contains("expands beyond"));
    }

    #[test]
    fn archive_reads_bound_actual_bytes_even_when_metadata_is_untrusted() {
        assert_eq!(bounded_read(Cursor::new(b"test"), 4).unwrap(), b"test");
        assert!(bounded_read(std::io::repeat(b'x'), 1024)
            .unwrap_err()
            .contains("exceeds"));
    }

    #[test]
    fn friendly_filename_hints_use_only_supported_semantic_categories() {
        let cases = [
            ("01_master_bio_note.md", "biography"),
            ("02_cv_2024.md", "cv"),
            ("09_therapy_notes.md", "therapy"),
            ("14_self_interview_childhood.md", "interview"),
            ("18_voice_memo_transcript.md", "notes"),
            ("19_linkedin_profile_export_text.md", "cv"),
        ];
        for (filename, expected) in cases {
            let classified = classify(
                filename,
                b"A long synthetic personal record with ordinary details about work, family, learning, decisions, routines, and plans.",
            )
            .unwrap();
            assert_eq!(classified.category, expected, "{filename}");
        }
    }

    #[test]
    fn misleading_extension_cannot_override_pdf_magic() {
        let minimal = b"%PDF-1.4\n% synthetic invalid body";
        let error = classify("notes.bin", minimal).unwrap_err();
        assert!(error.contains("PDF text extraction failed"));
    }

    #[test]
    fn parser_records_the_detected_subtype_not_an_untrusted_hint() {
        let bytes = b"A synthetic Markdown document with enough concrete words about work, family, learning, routines, interests, plans, decisions, places, and goals to create one deterministic record.";
        let parsed = parse("notes.md", bytes, "notes", "invented_format", "User").unwrap();
        assert_eq!(parsed.subtype, "markdown");
        assert_eq!(parsed.parser_id, "document_markdown");
    }

    #[test]
    fn container_structure_overrides_a_misleading_extension() {
        let mut output = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut output);
            let options = zip::write::SimpleFileOptions::default();
            archive.start_file("[Content_Types].xml", options).unwrap();
            archive
                .write_all(b"<Types><Override PartName=\"/word/document.xml\"/></Types>")
                .unwrap();
            archive.start_file("word/document.xml", options).unwrap();
            archive
                .write_all(b"<w:document><w:p>Synthetic notes</w:p></w:document>")
                .unwrap();
            archive.finish().unwrap();
        }
        let classification = classify("misleading.zip", &output.into_inner()).unwrap();
        assert_eq!(classification.subtype, "docx");
    }

    #[test]
    fn textual_structure_overrides_a_misleading_extension() {
        let html = classify(
            "notes.txt",
            b"<!doctype html><html><body>Synthetic notes</body></html>",
        )
        .unwrap();
        assert_eq!(html.subtype, "html");

        let fb2 = classify(
            "notes.txt",
            br#"<?xml version="1.0"?><FictionBook><body><section><p>Synthetic chapter</p></section></body></FictionBook>"#,
        )
        .unwrap();
        assert_eq!(fb2.subtype, "fb2");
        assert_eq!(fb2.category, "biography");
    }

    #[test]
    fn archive_path_traversal_is_rejected_during_detection() {
        let mut output = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut output);
            let options = zip::write::SimpleFileOptions::default();
            archive.start_file("../escape.md", options).unwrap();
            archive.write_all(b"Synthetic notes").unwrap();
            archive.finish().unwrap();
        }
        let error = classify("export.zip", &output.into_inner()).unwrap_err();
        assert!(error.contains("unsafe path"));
    }

    #[test]
    fn generic_text_archive_is_not_claimed_as_a_notion_export() {
        let mut output = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut output);
            let options = zip::write::SimpleFileOptions::default();
            archive.start_file("my_notion_notes.md", options).unwrap();
            archive.write_all(b"Synthetic notes").unwrap();
            archive.finish().unwrap();
        }
        let classification = classify("notion_export.zip", &output.into_inner()).unwrap();
        assert_eq!(classification.subtype, "archive_text");
    }

    #[test]
    fn facebook_detection_requires_export_structure_not_friendly_names() {
        let mut fake = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut fake);
            let options = zip::write::SimpleFileOptions::default();
            archive
                .start_file("my_profile_information_notes.md", options)
                .unwrap();
            archive.write_all(b"Synthetic notes").unwrap();
            archive.finish().unwrap();
        }
        assert_eq!(
            classify("facebook.zip", &fake.into_inner())
                .unwrap()
                .subtype,
            "archive_text"
        );

        let mut export = Cursor::new(Vec::new());
        {
            let mut archive = zip::ZipWriter::new(&mut export);
            let options = zip::write::SimpleFileOptions::default();
            archive
                .start_file("messages/inbox/friend/message_1.json", options)
                .unwrap();
            archive.write_all(br#"{"messages":[]}"#).unwrap();
            archive.finish().unwrap();
        }
        assert_eq!(
            classify("export.zip", &export.into_inner())
                .unwrap()
                .subtype,
            "facebook_export"
        );
    }
}
