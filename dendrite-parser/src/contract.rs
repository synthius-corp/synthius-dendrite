//! Public schema and semantic checks shared by every adapter and fixture.
use crate::{ParseResult, CORPUS_SCHEMA};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

pub const MAX_TEXT_CHARS: usize = 250_000;

fn schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        serde_json::from_str(include_str!("../../schemas/synthius-corpus-v1.schema.json"))
            .expect("bundled corpus schema")
    })
}

fn patterns() -> &'static BTreeMap<String, Regex> {
    static PATTERNS: OnceLock<BTreeMap<String, Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        fn collect(rule: &Value, result: &mut BTreeMap<String, Regex>) {
            if let Some(pattern) = rule["pattern"].as_str() {
                result.insert(
                    pattern.into(),
                    Regex::new(pattern).expect("bundled schema pattern"),
                );
            }
            if let Some(properties) = rule["properties"].as_object() {
                for child in properties.values() {
                    collect(child, result);
                }
            }
        }
        let mut result = BTreeMap::new();
        collect(schema(), &mut result);
        result
    })
}

// The bundled schema uses this deliberately small subset of Draft 2020-12.
// Fail closed on new validation keywords until their checks are implemented.
fn validate(value: &Value, rule: &Value, path: &str) -> Result<(), String> {
    for key in rule.as_object().ok_or("Invalid bundled schema")?.keys() {
        if !matches!(
            key.as_str(),
            "$schema"
                | "$id"
                | "title"
                | "description"
                | "type"
                | "additionalProperties"
                | "required"
                | "properties"
                | "minLength"
                | "maxLength"
                | "pattern"
                | "minimum"
                | "maximum"
        ) {
            return Err(format!("Unsupported corpus schema keyword: {key}"));
        }
    }
    let matches_type = |kind: &str| match kind {
        "object" => value.is_object(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => false,
    };
    let valid_type = match &rule["type"] {
        Value::String(kind) => matches_type(kind),
        Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).any(matches_type),
        _ => false,
    };
    if !valid_type {
        return Err(format!("Corpus field {path} has an invalid type"));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = rule["required"].as_array() {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(format!("Corpus field {path}.{name} is required"));
                }
            }
        }
        let properties = rule["properties"]
            .as_object()
            .ok_or("Invalid bundled object schema")?;
        for (name, child) in object {
            if let Some(child_rule) = properties.get(name) {
                validate(child, child_rule, &format!("{path}.{name}"))?;
            } else if rule["additionalProperties"] == false {
                return Err(format!("Unexpected corpus field {path}.{name}"));
            }
        }
    }
    if let Some(text) = value.as_str() {
        let count = text.chars().count() as u64;
        if rule["minLength"].as_u64().is_some_and(|min| count < min)
            || rule["maxLength"].as_u64().is_some_and(|max| count > max)
        {
            return Err(format!("Corpus field {path} exceeds its character bounds"));
        }
        if let Some(pattern) = rule["pattern"].as_str() {
            if !patterns()
                .get(pattern)
                .ok_or("Unregistered schema pattern")?
                .is_match(text)
            {
                return Err(format!("Corpus field {path} has an invalid representation"));
            }
        }
    }
    if value.is_i64() || value.is_u64() {
        let number = value.as_f64().ok_or("Invalid corpus number")?;
        if rule["minimum"].as_f64().is_some_and(|min| number < min)
            || rule["maximum"].as_f64().is_some_and(|max| number > max)
        {
            return Err(format!("Corpus field {path} exceeds its numeric bounds"));
        }
    }
    Ok(())
}

/// Validate one physical JSONL row, including citation coordinates and hash.
pub fn validate_corpus_record(value: &Value, row: usize) -> Result<(), String> {
    validate(value, schema(), "record")?;
    let metadata = &value["reference_metadata"];
    if metadata["record"].as_u64() != Some(row as u64) {
        return Err("Corpus reference record does not match its physical row".into());
    }
    let text = value["text"].as_str().ok_or("Corpus text is required")?;
    if text.trim().is_empty() {
        return Err("Corpus text must not be blank".into());
    }
    if value["user"]
        .as_str()
        .is_none_or(|name| name.trim().is_empty())
    {
        return Err("Corpus author must not be blank".into());
    }
    if metadata["text_sha256"].as_str()
        != Some(format!("{:x}", Sha256::digest(text.as_bytes())).as_str())
    {
        return Err("Corpus text fingerprint mismatch".into());
    }
    for unit in ["char", "line", "paragraph", "page"] {
        let start = metadata
            .get(format!("{unit}_start"))
            .and_then(Value::as_u64);
        let end = metadata.get(format!("{unit}_end")).and_then(Value::as_u64);
        if start.is_some() != end.is_some() || start.zip(end).is_some_and(|(a, b)| a > b) {
            return Err(format!("Invalid corpus {unit} coordinate pair"));
        }
    }
    Ok(())
}

/// Check the public parser result independently of adapter implementation.
pub fn validate_parse_result(result: &ParseResult, source: &[u8]) -> Result<(), String> {
    if source.is_empty()
        || source.len() > crate::MAX_SOURCE_BYTES
        || result.corpus.len() > crate::MAX_CORPUS_BYTES
        || result.message_count > crate::MAX_RECORDS
    {
        return Err("Source/corpus exceeds parser resource bounds".into());
    }
    if result.corpus_schema != CORPUS_SCHEMA
        || result.source_sha256 != format!("{:x}", Sha256::digest(source))
        || result.corpus_sha256 != format!("{:x}", Sha256::digest(&result.corpus))
    {
        return Err("Corpus/source identity mismatch".into());
    }
    if result.corpus.is_empty() || !result.corpus.ends_with(b"\n") {
        return Err("Corpus must contain newline-terminated JSONL rows".into());
    }
    let text = std::str::from_utf8(&result.corpus).map_err(|_| "Corpus is not UTF-8")?;
    let mut ids = BTreeSet::new();
    let mut stats: BTreeMap<String, (usize, usize, Option<String>, Option<String>)> =
        BTreeMap::new();
    let mut rows = 0;
    let mut words = 0;
    let mut dates = BTreeSet::new();
    for (index, line) in text.split_terminator('\n').enumerate() {
        let value: Value = serde_json::from_str(line).map_err(|_| "Invalid corpus JSONL row")?;
        validate_corpus_record(&value, index + 1)?;
        if !ids.insert(value["id"].as_str().unwrap().to_string()) {
            return Err("Duplicate corpus record ID".into());
        }
        rows += 1;
        let count = value["text"].as_str().unwrap().split_whitespace().count();
        words += count;
        let participant = stats
            .entry(value["user"].as_str().unwrap().into())
            .or_default();
        participant.0 += 1;
        participant.1 += count;
        if let Some(date) = value["ts"].as_str() {
            dates.insert(date.to_string());
            participant.2 = Some(
                participant
                    .2
                    .as_deref()
                    .map_or(date, |a| a.min(date))
                    .into(),
            );
            participant.3 = Some(
                participant
                    .3
                    .as_deref()
                    .map_or(date, |a| a.max(date))
                    .into(),
            );
        }
    }
    if rows != result.message_count
        || words != result.word_count
        || dates.first().cloned() != result.date_start
        || dates.last().cloned() != result.date_end
        || stats.len() != result.participants.len()
    {
        return Err("Corpus aggregate metadata mismatch".into());
    }
    let mut names = BTreeSet::new();
    for participant in &result.participants {
        if !names.insert(&participant.name)
            || stats.get(&participant.name)
                != Some(&(
                    participant.message_count,
                    participant.word_count,
                    participant.first_message.clone(),
                    participant.last_message.clone(),
                ))
        {
            return Err("Corpus participant metadata mismatch".into());
        }
    }
    Ok(())
}
