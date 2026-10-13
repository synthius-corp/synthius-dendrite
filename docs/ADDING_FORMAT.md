# Worked example: adding a deterministic source format

This fictional `.snote` format is a contribution tutorial, **not a currently
supported capability**. Use an entirely artificial file named `boat.snote`:

```text
SYNTH-NOTE/1
Synthetic paper boat sailed across the pond.
```

The file ends with one LF. Its UTF-8 source SHA-256 is
`1fd114b19e970d8cf3ab22141dc60059d55773cb9055f629a0fc88f1a9e81e7b`.
There are no accounts, credentials, absolute paths or network calls.

## Code locations

In `dendrite-parser/src/lib.rs`, add a capability:

```rust
ParserCapability {
    parser_id: "document_synthetic_note",
    extensions: &["snote"],
    subtypes: &["synthetic_note"],
},
```

In `classify()`, after the common empty/input-size guard, recognize the exact
`b"SYNTH-NOTE/1\n"` header before extension-based fallbacks. Return category
`notes`, subtype `synthetic_note`, confidence `1.0` and a structure hint. Reject
unsupported/truncated `SYNTH-NOTE/` versions explicitly, so they cannot be
silently treated as ordinary notes. A friendly filename alone must never claim
this format.

In `parse()`'s `detected_filename` match, map `synthetic_note` to `source.snote`.
In `extract_document_text()`, add the matching extraction arm:

```rust
".snote" => {
    let decoded = decode_text(bytes)?;
    let body = decoded.strip_prefix("SYNTH-NOTE/1\n")
        .ok_or_else(|| "Unsupported synthetic note version".to_string())?;
    if body.trim().is_empty() {
        return Err("Synthetic note contains no text".into());
    }
    Ok((normalized_plain_text(body), "document_synthetic_note"))
}
```

This reuses bounded document records and the shared emission validator. Do not
write your own account-aware output serializer or bypass the common limits.
For a chat/container format, implement its bounded record extraction in the
same module and feed the shared `Record`/emitter path instead.

## Exact expected output

With category `notes` and subject `Synthetic Person`, the exact single JSONL
row below ends in LF. It uses the existing shared serializer's key order:

```json
{"id":"d_1fd114b19e970d8c_0","reference_metadata":{"paragraph_end":1,"paragraph_start":1,"record":1,"text_sha256":"ac3bd3f5d2da43195682ba8e86fa4a19c38546549ea36a560ba10fac8db6669f","version":1},"source":"document","text":"Synthetic paper boat sailed across the pond.","ts":null,"type":"notes","uid":"document","user":"Synthetic Person"}
```

Read [the complete target contract](../schemas/README.md) for bounds, IDs,
coordinates, hashes, participants and dates. The result must declare
`document_synthetic_note`, `synthetic_note`, the chosen parser package version,
one message and seven words. The fixture golden also records the corpus hash,
source hash and participant totals.

## Fixtures and checks required in the PR

1. Commit `boat.snote` in `dendrite-parser/tests/fixtures/`. Add its filename,
   category, subtype, parser ID, synthetic marker and
   `expected/boat.snote.json` path to `manifest.json`. Every extension alias and
   every subtype needs its own positive entry; a subtype under another parser
   does not satisfy this adapter's coverage.
2. Register this adapter's `malformed`, `ambiguous` and `resource` cases in
   `negative-manifest.json`. The malformed case executes non-text input; the
   resource case executes a real input over 64 MiB; the ambiguous case should
   use `boat.snote` under `telegram-export.bin` and still detect/parse the
   fictional structure. The matrix requires all three classes for every parser.
3. Add adapter-specific integration tests in `dendrite-parser/tests/`: truncated
   and unsupported headers, empty body, a fake filename with ordinary content,
   misleading extension, Unicode, deterministic order and large-text splitting.
   A container adapter additionally tests actual expansion/member/path/nesting
   boundaries. Shared negative cases alone are not a sufficient format review.
4. Increase `dendrite-parser/Cargo.toml`'s version for the new behavior and
   refresh both lockfiles. Generate expected fixtures only after inspecting the
   output, using the explicit command in [CONTRIBUTING](../CONTRIBUTING.md).
   Commit every exact result under `fixtures/expected/`. PR CI rejects changed
   goldens without a version increase relative to the PR base.
5. Run the documented formatting, Clippy, Node and full workspace checks. The
   matrix runs full schema/citation/aggregate validation and compares exact
   expected JSONL/metadata. Parser-only tests can be run first with
   `cargo test -p synthius-dendrite-parser --locked`.
6. Update supported-format docs and open the PR using the acceptance checklist.
   @x-aero reviews the final diff and results, accepts it, and controls merging.
   Contributors do not receive production credentials or distribution rights.
