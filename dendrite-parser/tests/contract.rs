use serde_json::{json, Value};
use synthius_dendrite_parser::{
    parse, validate_corpus_record, validate_parse_result, MAX_TEXT_CHARS,
};

fn telegram(text: &str, author: &str) -> Value {
    json!({"messages":[{"type":"message","id":"synthetic-1","date":"2026-01-01",
        "from":author,"from_id":"synthetic-user","text":text}]})
}
fn rows(corpus: &[u8]) -> Vec<Value> {
    std::str::from_utf8(corpus)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn large_unicode_messages_are_split_without_losing_text_or_source_identity() {
    for size in [MAX_TEXT_CHARS, MAX_TEXT_CHARS + 1, 2 * MAX_TEXT_CHARS + 1] {
        let text = "🙂".repeat(size);
        let source = serde_json::to_vec(&telegram(&text, "Synthetic Person")).unwrap();
        let parsed = parse("export.json", &source, "chat", "telegram_json", "").unwrap();
        validate_parse_result(&parsed, &source).unwrap();
        let emitted = rows(&parsed.corpus);
        assert_eq!(emitted.len(), size.div_ceil(MAX_TEXT_CHARS));
        assert_eq!(
            emitted
                .iter()
                .map(|row| row["text"].as_str().unwrap())
                .collect::<String>(),
            text
        );
        for (index, row) in emitted.iter().enumerate() {
            assert_eq!(row["reference_metadata"]["original_id"], "synthetic-1");
            if size > MAX_TEXT_CHARS {
                assert_eq!(
                    row["reference_metadata"]["char_start"],
                    index * MAX_TEXT_CHARS
                );
                assert_eq!(
                    row["reference_metadata"]["char_end"],
                    ((index + 1) * MAX_TEXT_CHARS).min(size)
                );
            }
        }
    }
}

#[test]
fn blank_authors_are_normalized_and_metadata_limits_count_unicode_characters() {
    let source = serde_json::to_vec(&telegram("Synthetic message", " \t ")).unwrap();
    let parsed = parse("export.json", &source, "chat", "telegram_json", "").unwrap();
    assert_eq!(rows(&parsed.corpus)[0]["user"], "Unknown");
    validate_parse_result(&parsed, &source).unwrap();
    for (field, maximum) in [("from", 500), ("from_id", 500), ("id", 500), ("date", 128)] {
        let mut value = telegram("Synthetic message", "Synthetic Person");
        value["messages"][0][field] = json!("界".repeat(maximum));
        assert!(
            parse(
                "export.json",
                &serde_json::to_vec(&value).unwrap(),
                "chat",
                "telegram_json",
                ""
            )
            .is_ok(),
            "{field} boundary"
        );
        value["messages"][0][field] = json!("界".repeat(maximum + 1));
        assert!(
            parse(
                "export.json",
                &serde_json::to_vec(&value).unwrap(),
                "chat",
                "telegram_json",
                ""
            )
            .unwrap_err()
            .contains("character bounds"),
            "{field} overflow"
        );
    }
    assert!(parse(
        "notes.txt",
        b"Synthetic notes",
        &"x".repeat(121),
        "plain_text",
        "Person"
    )
    .is_err());
    assert!(parse(
        "notes.txt",
        b"Synthetic notes",
        "notes",
        "plain_text",
        &"x".repeat(501)
    )
    .is_err());
}

#[test]
fn public_validator_rejects_schema_and_citation_corruption() {
    let source = b"Synthetic ordinary notes";
    let parsed = parse("notes.txt", source, "notes", "plain_text", "Person").unwrap();
    validate_parse_result(&parsed, source).unwrap();
    let original = rows(&parsed.corpus)[0].clone();
    let mutations = [
        ("text", json!("")),
        ("user", json!(" ")),
        ("uid", json!(7)),
        ("id", json!("illegal id")),
        ("type", json!("x".repeat(121))),
        ("source", json!("x".repeat(121))),
        ("unknown", json!(true)),
    ];
    for (field, value) in mutations {
        let mut row = original.clone();
        row[field] = value;
        assert!(validate_corpus_record(&row, 1).is_err(), "{field}");
    }
    for (field, value) in [
        ("record", json!(2)),
        ("version", json!(-1)),
        ("text_sha256", json!("0".repeat(64))),
        ("char_start", json!(1)),
        ("page_start", json!(1_000_000_001)),
        ("unknown", json!(true)),
    ] {
        let mut row = original.clone();
        row["reference_metadata"][field] = value;
        assert!(validate_corpus_record(&row, 1).is_err(), "{field}");
    }
    let mut row = original;
    row["reference_metadata"]["char_start"] = json!(3);
    row["reference_metadata"]["char_end"] = json!(2);
    assert!(validate_corpus_record(&row, 1).is_err());
    let mut corrupt = parsed;
    corrupt.participants[0].word_count += 1;
    assert!(validate_parse_result(&corrupt, source).is_err());
}

#[test]
fn html_and_apple_notes_parse_by_structure_for_misleading_extensions() {
    for source in [
        b"<!doctype html><html><body><p>Synthetic document</p></body></html>".as_slice(),
        br#"<!doctype html><html><head><meta name="generator" content="Apple Notes"></head><body><p>Synthetic document</p></body></html>"#.as_slice(),
    ] {
        let expected = parse("notes.html", source, "notes", "html", "Person").unwrap();
        for filename in ["notes.txt", "notes.bin", "notes.json"] {
            let parsed = parse(filename, source, "notes", "html", "Person").unwrap();
            assert_eq!(parsed.parser_id, "document_html");
            assert_eq!(parsed.subtype, expected.subtype);
            assert_eq!(parsed.corpus, expected.corpus);
            validate_parse_result(&parsed, source).unwrap();
        }
    }
}
