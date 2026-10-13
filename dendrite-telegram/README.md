# Dendrite Telegram importer

Rust-only one-time history importer, pinned to Grammers 0.10.0. The desktop
runs this worker inside its own executable with `--telegram-import-worker`;
the standalone binary is for development/protocol tests. It accepts only
versioned, nonce-bound typed commands over stdin and emits bounded events on
stdout. Its session store is `MemorySession`; login data is never serialized.

Configure `SYNTHIUS_TELEGRAM_API_ID` and `SYNTHIUS_TELEGRAM_API_HASH` in the build
environment using your own registered Telegram application. Keep credentials
outside Git. The worker receives no Synthius bearer or LLM keys. Builds without
application credentials disable account access. Never invoke the standalone
worker with login secrets in arguments or capture raw protocol output in logs.

Run `cargo test --manifest-path Cargo.toml --locked --offline` from this directory
after dependencies have been fetched. Tests use synthetic messages and temporary
vaults, including generated-source parsing by the shared Dendrite parser.
The parent repository additionally tests the Tauri bridge, native UI and backend
ownership checks. Follow the parent `CONTRIBUTING.md` and `SECURITY.md` for changes.

QR/phone/2FA and confirmed remote logout still require an explicitly authorized
dedicated Telegram test account. Memory-only storage reduces persistence; it
does not protect a session from privileged device compromise or OS paging.
