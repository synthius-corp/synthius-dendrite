# Public corpus acceptance contract

The parser emits UTF-8, newline-terminated `synthius-corpus/v1` JSONL. Every
physical row must satisfy [the schema](synthius-corpus-v1.schema.json) and
`validate_corpus_record(row, physical_row)`. `validate_parse_result(result,
source_bytes)` additionally verifies the complete corpus/source hashes, unique
IDs, record and word totals, participants and lexical date ranges. These public
Rust functions need no account, provider credentials or private backend.

The validator implements every validation keyword currently used in the schema
and fails closed if an unsupported validation keyword is added. The integration
tests corrupt real output to check rejection. The consuming host independently
validates the same contract; passing here does not authorize an upload.

## Generous bounds and normalization

Source and corpus limits remain **64 MiB each**, **250,000 emitted records**,
**10,000 archive members** and **256 MiB declared archive expansion**. Supported
archive members remain individually bounded by 64 MiB; nested archives are not
recursively extracted. Limits apply before selecting an adapter and during
emission. New formats must enforce any additional format-specific allocation
or expansion limits.

Text may contain **250,000 Unicode scalar values per row**. A longer accepted
message/document is deterministically split at that boundary rather than
truncated or rejected merely because it is one long message. Blank chunks are
omitted. Names, participant UIDs and original IDs allow 500 characters;
timestamps allow 128; type/source allow 120. Metadata that exceeds the existing
compatible bounds is rejected with a field error rather than silently altered.
Character limits count Unicode scalar values, not UTF-8 bytes. Blank authors
become `Unknown` (blank document subject defaults to `User`).

## Meaning of fields and references

- `uid` identifies a source participant, never a Synthius account or persona.
- `type` is the record kind/category; `source` is its source family. Result
  `parser_id` identifies the technical adapter; result `subtype` is the detected
  source subtype, not an untrusted caller hint.
- `id` is deterministic: source-hash prefix plus zero-based emitted position.
  `reference_metadata.record` is the **one-based physical JSONL row**.
- Reference version is currently 1. `original_id` preserves the source event ID
  when available; split parts retain that ID and get distinct emitted IDs.
- `text_sha256` hashes the exact UTF-8 emitted text.
- Line and paragraph ranges are one-based inclusive ranges of the decoded or
  extracted source used by the adapter. Paragraphs come from blank-line-separated
  extracted text; extraction may normalize the original document markup.
- Character ranges on split records are zero-based, half-open Unicode scalar
  offsets within that original record's normalized text. Enclosing source
  line/paragraph ranges are retained. They are not byte offsets in a ZIP/PDF.
- Every coordinate start requires its end; start must not exceed end. Page
  coordinates, when supplied by an adapter, are one-based inclusive pages.
- Timestamps are nullable source representations, not a mandatory ISO timestamp.
  Date aggregates use lexical ordering of those retained representations.

Output and metadata must be deterministic for identical source bytes, parser
version and options. No clocks, randomness, network requests or machine paths
may enter output. The committed expected fixture results include the full corpus,
hashes, version, counts, participants and dates. A deliberately changed output
requires an explicit parser-version decision and owner review of the golden diff.
