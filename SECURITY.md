# Security policy

Do not open a public issue for a vulnerability that may expose source content,
cross tenant or persona boundaries, escape the local vault, bypass parser limits,
or compromise the update channel. Use GitHub's **Security → Report a vulnerability**:
[private vulnerability report](https://github.com/synthius-corp/synthius-dendrite/security/advisories/new).
Include the affected version, an entirely synthetic reproduction and the impact.
Never attach real exports, credentials or account histories.

GitHub private vulnerability reporting is the reporting route. If GitHub does
not offer the private report form, keep the details private; do not use an issue
as a fallback. No email or external contact page is used as a reporting destination.

Dendrite stores no Synthius service-role key or reusable LLM/tool credential. Its localhost API requires a validated owner token, restricts CORS to compiled Synthius origins, and resolves paths beneath its fixed vault. Community parser code is deterministic and credential-free.

The Telegram snapshot worker temporarily holds a broad Telegram account session
in a Grammers `MemorySession`. It uses the native bundled login window only;
the web app receives content-free progress and file manifests. The worker's
environment is cleared except for the non-secret Windows runtime-root values
required by native networking. No credentials are passed in process arguments,
and session keys never enter source checkpoints or registration payloads. Normal
termination attempts bounded logout. Offline/crash cleanup may be unconfirmed,
which is recorded and shown with Telegram Devices revocation guidance. Fresh
login is mandatory on resume.

Read-only operation is an application allowlist, not a reduced Telegram grant.
The child is isolated from host tokens but is not an OS filesystem sandbox.
Privileged malware, SDK/WebView transient copies, swap and crash dumps remain
risks. Review authentication IPC, dependency updates, output hashes and fresh
owner/persona/device binding when changing the importer. Never use real accounts,
login material or histories as public test fixtures.

Automatic updates are currently disabled in official release profiles. Before
enabling them, desktop updates must be initiated by Rust and require HTTPS, a trusted artifact
signature, a signature-bound version and a strictly newer SemVer. The hosted
web app has no updater permission. Loopback update mutations require an exact
origin and an unguessable per-process token. Signing private keys belong only in
the release secret manager and must never enter the repository, app bundle,
logs or update manifest.
