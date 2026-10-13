use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use synthius_dendrite_parser::{
    capabilities, classify, parse, validate_parse_result, ParseResult, CORPUS_SCHEMA,
};

#[derive(Debug, Deserialize)]
struct Fixture {
    filename: String,
    category: String,
    subtype: String,
    parser_id: String,
    contains: String,
    expected: String,
}

fn golden(result: &ParseResult) -> Value {
    let mut value = serde_json::to_value(result).unwrap();
    value["corpus"] = json!(std::str::from_utf8(&result.corpus).unwrap());
    value
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

#[test]
fn every_declared_format_has_a_synthetic_classification_and_parse_fixture() {
    let root = fixture_root();
    let fixtures: Vec<Fixture> =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let mut fixture_names = BTreeSet::new();
    let mut covered_parsers = BTreeSet::new();
    let mut covered_parser_extensions = BTreeSet::new();
    let mut covered_subtypes = BTreeSet::new();

    for fixture in fixtures {
        assert!(
            fixture_names.insert(fixture.filename.clone()),
            "duplicate fixture {}",
            fixture.filename
        );
        let bytes = fs::read(root.join(&fixture.filename)).unwrap();
        assert!(!bytes.is_empty(), "empty fixture {}", fixture.filename);

        let classification = classify(&fixture.filename, &bytes).unwrap();
        assert_eq!(
            classification.category, fixture.category,
            "{}",
            fixture.filename
        );
        assert_eq!(
            classification.subtype, fixture.subtype,
            "{}",
            fixture.filename
        );

        let first = parse(
            &fixture.filename,
            &bytes,
            &classification.category,
            &classification.subtype,
            "Synthetic Person",
        )
        .unwrap();
        let second = parse(
            &fixture.filename,
            &bytes,
            &classification.category,
            &classification.subtype,
            "Synthetic Person",
        )
        .unwrap();
        assert_eq!(first.parser_id, fixture.parser_id, "{}", fixture.filename);
        assert_eq!(first.subtype, fixture.subtype, "{}", fixture.filename);
        assert_eq!(first.corpus_schema, CORPUS_SCHEMA);
        assert!(
            first.message_count > 0,
            "{} emitted no records",
            fixture.filename
        );
        assert_eq!(
            first.corpus, second.corpus,
            "{} was nondeterministic",
            fixture.filename
        );
        assert_eq!(first.corpus_sha256, second.corpus_sha256);
        validate_parse_result(&first, &bytes).unwrap();
        let expected: Value =
            serde_json::from_slice(&fs::read(root.join(&fixture.expected)).unwrap()).unwrap();
        assert_eq!(golden(&first), expected, "{} changed its expected corpus/metadata; inspect the output and parser version before updating", fixture.filename);
        let corpus = String::from_utf8(first.corpus).unwrap();
        assert!(
            corpus.contains(&fixture.contains),
            "{} did not preserve its synthetic marker",
            fixture.filename
        );
        let extension = fixture
            .filename
            .rsplit_once('.')
            .map(|(_, value)| value.to_ascii_lowercase())
            .expect("fixture filename must include an extension");
        covered_parser_extensions.insert((fixture.parser_id.clone(), extension));
        covered_parsers.insert(fixture.parser_id.clone());
        covered_subtypes.insert((fixture.parser_id.clone(), fixture.subtype));
    }

    for capability in capabilities() {
        assert!(
            covered_parsers.contains(capability.parser_id),
            "parser {} has no synthetic fixture",
            capability.parser_id
        );
        for subtype in capability.subtypes {
            assert!(
                covered_subtypes.contains(&(capability.parser_id.to_string(), subtype.to_string())),
                "subtype {subtype} has no synthetic fixture"
            );
        }
        for extension in capability.extensions {
            assert!(
                covered_parser_extensions
                    .contains(&(capability.parser_id.to_string(), extension.to_string())),
                "parser {} extension .{} has no synthetic fixture",
                capability.parser_id,
                extension
            );
        }
    }
}

#[derive(Deserialize)]
struct NegativeFixture {
    parser_id: String,
    case: String,
    filename: String,
    source_fixture: Option<String>,
    expected_subtype: Option<String>,
}

#[test]
fn every_adapter_has_malformed_ambiguous_and_resource_cases() {
    let root = fixture_root();
    let cases: Vec<NegativeFixture> =
        serde_json::from_slice(&fs::read(root.join("negative-manifest.json")).unwrap()).unwrap();
    let mut covered = BTreeSet::new();
    // One reusable allocation exercises the real generous production boundary.
    let oversized = vec![b'x'; 64 * 1024 * 1024 + 1];
    for case in cases {
        assert!(
            covered.insert((case.parser_id.clone(), case.case.clone())),
            "duplicate negative case"
        );
        let bytes = match case.case.as_str() {
            "malformed" => vec![0, 0, 0, 0],
            "resource" => {
                assert!(classify(&case.filename, &oversized).is_err());
                assert!(parse(
                    &case.filename,
                    &oversized,
                    "notes",
                    "untrusted",
                    "Synthetic Person"
                )
                .is_err());
                continue;
            }
            "ambiguous" => fs::read(
                root.join(
                    case.source_fixture
                        .as_ref()
                        .expect("ambiguous source fixture"),
                ),
            )
            .unwrap(),
            _ => panic!("unknown mandatory case class"),
        };
        if case.case == "malformed" {
            assert!(
                classify(&case.filename, &bytes).is_err(),
                "{} malformed classification",
                case.parser_id
            );
            assert!(
                parse(
                    &case.filename,
                    &bytes,
                    "notes",
                    "untrusted",
                    "Synthetic Person"
                )
                .is_err(),
                "{} malformed parsing",
                case.parser_id
            );
        } else {
            let classification = classify(&case.filename, &bytes)
                .unwrap_or_else(|error| panic!("{} {}: {error}", case.parser_id, case.filename));
            assert_eq!(
                Some(&classification.subtype),
                case.expected_subtype.as_ref()
            );
            let parsed = parse(
                &case.filename,
                &bytes,
                &classification.category,
                &classification.subtype,
                "Synthetic Person",
            )
            .unwrap();
            assert_eq!(parsed.parser_id, case.parser_id);
            validate_parse_result(&parsed, &bytes).unwrap();
        }
    }
    for capability in capabilities() {
        for case in ["malformed", "ambiguous", "resource"] {
            assert!(
                covered.contains(&(capability.parser_id.to_string(), case.to_string())),
                "parser {} lacks {case} coverage",
                capability.parser_id
            );
        }
    }
}

#[test]
#[ignore = "explicit maintainer-only golden update; review the diff and version decision"]
fn regenerate_goldens() {
    assert_eq!(std::env::var("DENDRITE_UPDATE_GOLDENS").as_deref(), Ok("1"));
    let root = fixture_root();
    let fixtures: Vec<Fixture> =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    for fixture in fixtures {
        let bytes = fs::read(root.join(&fixture.filename)).unwrap();
        let result = parse(
            &fixture.filename,
            &bytes,
            &fixture.category,
            &fixture.subtype,
            "Synthetic Person",
        )
        .unwrap();
        validate_parse_result(&result, &bytes).unwrap();
        let output = root.join(&fixture.expected);
        fs::create_dir_all(output.parent().unwrap()).unwrap();
        fs::write(
            output,
            serde_json::to_string_pretty(&golden(&result)).unwrap() + "\n",
        )
        .unwrap();
    }
}
