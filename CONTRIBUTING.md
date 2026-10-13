# Contributing to Synthius Dendrite

Dendrite accepts deterministic technical ingestion improvements: new source formats, safer detection, parsing fidelity, corpus coordinates, resource limits and local-index interoperability. Persona prompts, model behavior, cloud credentials and private application logic are out of scope.

Fork the repository, create a branch in your fork, make the smallest complete
change, and open a PR. Contributions are suggestions: **only @x-aero accepts
and merges changes or starts official distribution workflows**. Passing tests
does not grant merge/release access. All files, including workflows, schemas and
expected outputs, are owned by @x-aero in `.github/CODEOWNERS`; required GitHub
protection rules must be active before public contributions are enabled.

Install Rust 1.92.0 and Node.js 24. Work from this standalone repository's root;
no parent Synthius checkout is needed. Parser contributors need no Docker,
account, Telegram application identity, signing key or provider access.
Desktop contributors can also run `npm run test:desktop`,
`npm run build:offline` and `npm run dev:offline` without a Supabase instance or
the private app. These commands use a nonfunctional public-key fixture, isolated
local-dev identity/storage and disabled updates. They do not bypass authentication;
live account-backed behavior is a separate maintainer integration check.

## Add a format

1. Add one narrowly scoped adapter to `dendrite-parser` and declare it in `capabilities()`.
2. Detect structure or magic before MIME/extension. Filename wording may only be a low-confidence semantic hint.
3. Emit stable ordered `synthius-corpus/v1` records with non-empty UTF-8 text and source coordinates. Never emit account IDs, persona IDs, absolute paths or credentials.
4. Add at least one committed, entirely synthetic source file for every new
   subtype and every declared extension alias in `dendrite-parser/tests/fixtures`,
   register it in `manifest.json`, and prove both exact classification and
   parsing in the fixture matrix. A parser capability without this executable
   example is incomplete. Also add
   ambiguous and malformed fixtures; include wrong-extension and adversarial
   archive cases where relevant.
5. Prove bounded input, expanded archive size, member count, nesting, output bytes and record count.
6. Register mandatory malformed, ambiguous and resource cases per parser in
   `negative-manifest.json`. Include format-specific malformed and expansion
   cases in addition to the shared limits. Commit the exact expected corpus and
   result metadata under `fixtures/expected/`, referenced by `manifest.json`.
   CI checks the full public schema and citation/aggregate semantics, exact
   output, every parser/subtype pair and every extension alias.
7. Run the commands below and include the results in the PR. Review every output
   change and bump the parser version when behavior changes. Never refresh
   expected output just to make a failing test green.

Read the [worked format example](docs/ADDING_FORMAT.md) and
[full target contract](schemas/README.md). The format example identifies every
code/manifest/test location and shows exact artificial input and expected JSONL.

```sh
npm test
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

To review a deliberate golden update locally (not in ordinary CI), set
`DENDRITE_UPDATE_GOLDENS=1` and run
`cargo test -p synthius-dendrite-parser --test fixture_matrix regenerate_goldens -- --ignored`.
Remove the environment value afterward and inspect every changed golden. The
owner reviews the exact final code, fixtures, version and expected-output diff.
Desktop changes also require `cargo fmt --manifest-path src-tauri/Cargo.toml
--all --check` and `npm run test:desktop`.

Official releases additionally pass Synthius's private corpus-compatibility and tenant-boundary suites. Dendrite does not load third-party adapters at runtime; accepted adapters ship only in reviewed official releases. Platform signing and notarization are release-publisher operations, not privileges granted to parser code.

## Native source upload UI

The bundled desktop dialog lives in `src-tauri/src/status.html` and `ui/uploads.js`.
The UMD `ui/source-selection.cjs` and its TypeScript declaration are shared with
the host web Sources page through the dependency-free
`@synthius/dendrite-source-selection` package. Official releases publish its
small dependency-free npm tarball; the host vendors and integrity-locks a
reviewed release instead of requiring repository/registry credentials in
frontend builds. Bump the helper
package version and refresh the host dependency when its behavior changes.
Keep file/folder collection, path-display normalization,
resource limits and batch retry behavior in that common module rather than
duplicating host logic. Native orchestration uses the existing Rust parser and
versioned online upload adapters; this public tree imports no private backend
or web application modules. Never expose the upload capability to JavaScript,
persist it outside the OS credential store, or allow UI-supplied backend URLs.
Add synthetic executable coverage for every UI, retry and security change.

## Telegram importer changes

The `dendrite-telegram` crate contains a narrow Rust/Grammers adapter, independent
of deterministic parsers. Its native login UI, host coordinator and cleanup
remain in this open-source tree. Keep sessions memory-only, login IPC native-only,
account/persona/device scope fixed, and provider operations limited to the
existing typed allowlist. Add synthetic tests for every normalization change
and security/recovery branch. Run the workspace tests and desktop-host tests;
Synthius integration additionally runs frontend and backend authorization tests.
An explicitly authorized dedicated-account test is required for auth/protocol
upgrades. Never add real Telegram content or application credentials to fixtures.

## Determinism

Identical bytes, parser version and options must produce byte-identical ordered JSONL. Do not include wall-clock time, random IDs, machine paths, locale-sensitive ordering or network results. New behavior requires a parser version change when it changes output.

## CI and dependency changes

CI runs tooling checks for every reviewed PR. Documentation-only changes avoid
the native platform matrix; code, dependencies, fixtures, workflows and unknown
paths require all four platforms. `CI acceptance` is the stable merge check and
rejects stale head/base results. New PR commits cancel the earlier PR test run.
Outside contributors must receive owner approval before workflows run once
public contributions are enabled; approval to test grants no merge/release right.

When changing Cargo dependencies or lockfiles, run `npm run licenses` after Cargo
has downloaded the locked dependencies, and commit the updated distribution
notices. The command prepares license/source information without compiling or
publishing. `npm test` rejects stale notice receipts. Retain dependency notices;
new fixtures and examples must be synthetic and yours to contribute.

## Security

Treat every source as hostile. Reject path traversal, archive bombs, unsupported recursion, oversized members, invalid structures and unbounded metadata. Do not log source content or filenames. Report vulnerabilities privately as described in `SECURITY.md`.
