# synthius-dendrite-parser

Credential-free deterministic detection and conversion of supported sources to `synthius-corpus/v1` JSONL. This crate is used by the Dendrite desktop app and is packaged as the exact pinned parser executable used by Synthius cloud ingestion.

```powershell
cargo run -p synthius-dendrite-parser -- capabilities
cargo run -p synthius-dendrite-parser -- classify .\source.md
cargo run -p synthius-dendrite-parser -- parse .\source.md notes markdown "Subject" .\corpus.jsonl
```

The JSON capability catalogue is the format-extension point. PostgreSQL does not enumerate adapter subtypes. See `../CONTRIBUTING.md` for the adapter contract and required tests.

Every advertised capability, subtype and extension alias has a committed
artificial source in `tests/fixtures/manifest.json`. The acceptance test
classifies and parses every fixture twice, checks the declared parser identity
and proves deterministic `synthius-corpus/v1` output. New formats are not
accepted without extending this matrix.
