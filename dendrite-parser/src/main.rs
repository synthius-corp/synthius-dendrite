use serde_json::json;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::Path;
use synthius_dendrite_parser::{capabilities, classify, parse};

fn usage() -> ! {
    eprintln!("usage: dendrite-parser capabilities | classify <source> | parse <source> <category> <subtype> <subject> <corpus-output>");
    std::process::exit(2);
}

fn main() {
    if let Err(message) = run() {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("capabilities") if args.len() == 2 => {
            println!(
                "{}",
                serde_json::to_string(&capabilities())
                    .map_err(|_| "capability serialization failed")?
            );
        }
        Some("classify") if args.len() == 3 => {
            let path = Path::new(&args[2]);
            let bytes = fs::read(path).map_err(|_| "source could not be read".to_string())?;
            let filename = path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or("source filename is invalid")?;
            println!(
                "{}",
                serde_json::to_string(&classify(filename, &bytes)?)
                    .map_err(|_| "classification serialization failed")?
            );
        }
        Some("parse") if args.len() == 7 => {
            let path = Path::new(&args[2]);
            let output = Path::new(&args[6]);
            let bytes = fs::read(path).map_err(|_| "source could not be read".to_string())?;
            let filename = path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or("source filename is invalid")?;
            let result = parse(filename, &bytes, &args[3], &args[4], &args[5])?;
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)
                    .map_err(|_| "corpus output directory could not be created".to_string())?;
            }
            fs::write(output, &result.corpus)
                .map_err(|_| "corpus output could not be written".to_string())?;
            let output_sha256 = format!("{:x}", Sha256::digest(&result.corpus));
            println!(
                "{}",
                json!({
                    "parser_id": result.parser_id,
                    "subtype": result.subtype,
                    "parser_version": result.parser_version,
                    "corpus_schema": result.corpus_schema,
                    "corpus_sha256": output_sha256,
                    "source_sha256": result.source_sha256,
                    "message_count": result.message_count,
                    "word_count": result.word_count,
                    "participants": result.participants,
                    "date_start": result.date_start,
                    "date_end": result.date_end,
                })
            );
        }
        _ => usage(),
    }
    Ok(())
}
