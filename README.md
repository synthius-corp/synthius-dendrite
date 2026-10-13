# Synthius Dendrite

Thin local source runtime for Synthius accounts that mix local and cloud sources.

Canonical source repository: <https://github.com/synthius-corp/synthius-dendrite>.
Community contributions use fork pull requests; the owner reviews and accepts upstream changes.
Clone this repository directly for desktop/parser development. Synthius consumes
reviewed versioned artifacts; the desktop and cloud share the same Rust parser.

## Standalone checks

Install Rust 1.92.0 and Node.js 24. On Linux, also install the Tauri prerequisites
listed in `.github/workflows/ci.yml`. From the repository root:

```sh
npm test
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
npm run test:desktop
```

Every parser capability includes synthetic examples and executable detection,
parsing and resource-limit tests. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Overview

Synthius Dendrite keeps file-backed persona sources on the local filesystem in plaintext and exposes a minimal localhost runtime for:

- local health checks
- local file upload, download, delete, and stats operations
- deterministic local classification and JSONL parsing through the reusable `synthius-dendrite-parser` package
- a bundled **Add** dialog for file/folder selection, drag-and-drop batches,
  category/subject review, source-use selection, local parsing and resumable progress
- local source-search index storage and keyword, grep, restricted regex,
  semantic and hybrid retrieval through an authenticated outbound relay
- an authenticated, content-free catalogue of all account sources, with Cloud
  and Local residence shown distinctly, compact source-purpose controls and
  deletion through the shared source lifecycle
- tray icon, logs, and storage folder access
- signed in-app update checks and user-started patch installation
- one-time Telegram user-history snapshots through a bundled Rust native
  authorization window, with memory-only sessions and local source files

The app does not run the Python backend or LLM pipeline locally. Dendrite parses
raw local sources and keeps the source-search index beside local source bundles;
validated corpus processing, metadata and model jobs remain online.

## Supported ingestion contracts

The built-in capability catalogue currently covers Telegram JSON, WhatsApp
text, ordinary text/Markdown/HTML, narrative JSON documents, PDF, DOCX, RTF, ODT, EPUB, FB2, bounded
text archives, and recognized LinkedIn/Instagram/Facebook export containers.
Container signatures and bounded structure take precedence over extensions or
filenames. Run `cargo run -p synthius-dendrite-parser -- capabilities` for the
machine-readable list. Generic textual ZIPs remain `archive_text`; a suggestive
filename is not treated as proof of a vendor export.

New formats belong in the credential-free `dendrite-parser` crate. Follow
[CONTRIBUTING.md](CONTRIBUTING.md): add a bounded deterministic adapter,
capability declaration, synthetic positive/ambiguous/malformed fixtures and
wrong-extension/adversarial coverage. Community code reaches desktop and cloud
parsing only through one reviewed, pinned official parser release. Every output
row must satisfy [`schemas/synthius-corpus-v1.schema.json`](schemas/synthius-corpus-v1.schema.json)
and the backend's independent, stricter manifest/hash validation.

The [public contract](schemas/README.md) defines the generous bounds and exact
citation semantics. The [worked format example](docs/ADDING_FORMAT.md) shows
where to add an adapter, synthetic inputs, exact expected output and required
tests. Only @x-aero accepts upstream changes and starts official distribution
workflows; see [owner controls](docs/OWNER_CONTROLS.md).

## Runtime model

- Source files: stored locally under the desktop runtime storage root.
- Metadata and job state: stored in Supabase.
- Parsing: performed locally for local sources, without model calls.
- Source search index: stored locally for a local source and migrated with its source and corpus.
- Source retrieval: Dendrite verifies and searches its own index; bounded
  snippets and explicitly fetched passages return to the online agent through
  an outbound WebSocket. Stored document vectors and index artifacts stay local.
- Processing: validated corpus processing and model inference are performed by the cloud backend.
- Output lifecycle: completed outputs can finish in cloud storage while the device is offline, then sync into local storage when the runtime reconnects. The cloud copy is removed only after local materialization succeeds.
- The web app talks to `http://127.0.0.1:1543` only for health, owner-scoped
  filesystem/index persistence, classification, parsing and content-free
  Telegram import intents/manifests. Login fields use trusted native IPC.

## Telegram snapshots

In Dendrite's **Telegram** tab, select the target **Persona** and source purpose,
then choose **Start a snapshot**. Dendrite verifies that account's persona and
device and opens its bundled native login and chat/topic/date selector directly.
Before first use, the native window explains selected-content AI processing and
asks for **I agree and continue**. Content is not used to train LLMs. Acceptance
is saved once per Synthius account and survives updates, reinstalls and use of
another device or persona. **Not now** starts no Telegram connection. After
acceptance, each snapshot still requires the user's own chat/topic/date selection.
Each import uses fresh authorization; completion, pause and cancellation attempt
logout. A paused snapshot resumes after a new login to the same Telegram account.
An unconfirmed logout directs the user to Telegram Settings → Devices.

Completed snapshots become ordinary Local JSON sources automatically. Dendrite
verifies the immutable file proof, registers and parses the source for the selected
persona, completes its search-index handoff, and refreshes the Sources catalogue
before showing the persona link. Registration and preparation
is idempotent, so reopening or retrying after an interrupted acknowledgement does
not create another source. Follow the link to the persona and choose **Process new
info** in Synthius when you want a persona update. Source search and per-file
migration reuse the existing source lifecycle.

The `dendrite-telegram` crate and native `ui/telegram.*` are part of this repository.
The desktop runs the same binary in a dedicated worker mode, with a cleared
environment except for the non-secret Windows runtime-root variables required
by native networking, and private versioned stdio IPC. Grammers 0.10.0 owns
Telegram protocol/cryptography; `MemorySession` is the only session store. The
worker receives no Synthius bearer token or model key. Local checkpoints store source
content and progress, not Telegram session material. They are removed as soon
as final source files are durable, with idempotent cleanup recovery after a
crash or temporary file lock. Process isolation is not an OS sandbox;
compromised devices and OS paging remain residual risks.

### Configure a development Telegram build

Create your application credentials through [Telegram application registration](https://core.telegram.org/api/obtaining_api_id).
For a community build with live Telegram import, set your own
`SYNTHIUS_TELEGRAM_API_ID` and `SYNTHIUS_TELEGRAM_API_HASH` in the build process
environment, then run `npm run build:production`. This command loads the public
Production client configuration automatically; sign in with your own Synthius
and Telegram accounts. You do not need Synthius's Telegram application credentials.
Keep values out of Git, logs and command arguments. These identify your Telegram
application and are embedded in its binary; user authorization remains a separate
memory-only session. Without the pair, the app still compiles and runs, but live
Telegram login/import is unavailable. Parsing saved exports does not require them.
The `test:desktop`, `build:offline` and `dev:offline` commands deliberately omit
Telegram credentials for isolated development. No backend
Telegram session service is needed. The configured Synthius backend must support
authenticated account-consent GET/POST endpoints at
`/api/files/telegram-import-consent` (web JWT) and
`/api/dendrite/uploads/telegram-consent` (paired upload capability). Both return
`{user_id, accepted, accepted_at}`; POST requires exactly `{"accepted": true}`
and must preserve the original receipt. A failed lookup/save keeps the native
window before login. Account acceptance has no app-version, device or persona key.

Official desktop releases include Telegram import using the application identity
from the protected release environment. The builder rejects a missing/invalid
identity rather than publishing an accidentally unavailable importer. There is
no Telegram feature switch or separate policy-approval flag. See
[application identity and consent](docs/OWNER_CONTROLS.md#telegram-application-identity-and-consent).

The initial importer accepts text/captions from selected retained histories.
The native selector prevents choosing more than 20 chat/topic pairs and disables
start until the dates and selection are valid. Large selected histories have no
artificial total-message, checkpoint-byte or wall-clock cutoff: pages remain
bounded and crash-safe, while completed output is split into sources of at most
8 MiB. Protected, secret and auto-delete content is excluded. Older basic groups
and their replacement supergroups are separate explicit selections.
Live QR/phone/2FA, logout and packaged-platform tests are release checks; offline
fixtures do not prove them. Telegram's [API terms](https://core.telegram.org/api/terms)
also apply to how imported material is used.

## Installation

### Windows

1. Download the latest Synthius Dendrite Windows installer from releases.
2. Run the installer.
3. The app appears in the system tray and starts the local runtime automatically.
4. In the Dendrite window, click **Open Synthius** and sign in in your browser
   once to connect the account. Return to Dendrite and choose **Add local source**.
   Choose its persona, select files/folders or drag them in, review their categories
   and confirm parsing. The complete Add workflow stays in the desktop window;
   ordinary web uploads are cloud sources. Move an individual file through its
   Sources row when you want to change its storage location.

### macOS

1. Download the latest Synthius Dendrite macOS image for your architecture from releases.
2. Drag the app into Applications.
3. Launch it once so it appears in the menu bar and starts the local runtime.
4. Click **Open Synthius** and sign in once to connect the account. Return to
   Dendrite, choose **Add local source**, select its persona and files/folders,
   review their categories, and confirm parsing. Drag-and-drop and batch controls
   are also available. Ordinary web uploads are cloud sources.

The landscape window has three sections: **Overview** for health and getting
started, **Sources** for the cached account file list and per-file moves, and
**Telegram** for one-time snapshots. Sources can be filtered by All, Local or
Cloud, with Local selected initially. **Add** at the top-right uses the same local
upload flow as Overview. The eye beside a Local source opens the folder containing
that exact file on this computer. The version/update footer stays visible at the
bottom of every section.
Local and Cloud rows use green and blue left-edge gradients, identified by the
legend above the list. The Overview count and size include original local source
files on this device only; parsed corpus, indexes, interview records and persona
output files are excluded.
Launching Dendrite again brings the existing window forward, including when
minimized or tray-hidden; one desktop runtime serves the installation.

Native Add supports 500 files per batch and 50 MiB per original. It shares the
web Sources selection helpers, deterministic Rust parser and existing online
registration/corpus/index APIs. Persona sources join the processing queue;
each new Add dialog defaults to **Persona source and context**. Context-only
sources support search. Saved source progress survives restart;
unfinished files that have not yet been copied need selection again. Retry keeps
the source UUID and batch identity and resumes a committed index transfer.

The initial web connection issues an exact-account/device, upload-only capability
valid for 30 days. Rust keeps it in Windows Credential Manager or macOS Keychain;
its expiry/revocation requires reconnecting the account. The private desktop
page receives a process-bound local token, with account/provider credentials
kept outside JavaScript. Backend adapters revalidate owner/persona/device and
reuse the host application's handlers. Their versioned contract is independent
of private repository imports. No schema migration or new provider key is needed.

## Tray menu

- Local Runtime status
- Open Dendrite status window
- Open Synthius web app in the default browser
- Open storage folder
- Open logs folder
- Quit

## Log file locations

Use the tray or menu-bar action to open the logs folder directly.

Log files:

- `sidecar.log`: local runtime startup and request-level diagnostics
- `tauri.log`: Tauri lifecycle logs

## Storage location

Click the **Local files** count in the Dendrite window, or use the tray/menu-bar
action, to open the configured storage folder. Within that root, files are
separated into `uploads/` and `outputs/`. The action opens the fixed configured
folder; web pages cannot provide another path.

## Development

### Run Synthius Dendrite locally

```powershell
npm ci
npm run dev:offline
```

This selects the isolated local-dev identity and an explicit nonfunctional
public-key fixture. Parser/UI/native tests and desktop compilation need no
running Supabase, private backend or Docker. Account-backed features need a
separately configured live environment; the fixture cannot authenticate or
grant access to user data. Use `npm run build:offline` to compile the debug
desktop executable without an installer or launching it.

### Test against the fully local web app

Start an isolated localhost Supabase project and export its API URL and public
key as `SYNTHIUS_SUPABASE_URL` and `SYNTHIUS_SUPABASE_PUBLIC_KEY`. Install the
locked tools with `npm ci`, then run `./build_local_dev.ps1` on Windows. It
accepts the isolated HTTP project at `127.0.0.1:54321` or `localhost:54321`.
Launch `dist/local-dev/synthius-dendrite.exe`. This build validates tokens
against local Supabase at `127.0.0.1:54321` and **Open Synthius** opens
`http://localhost:3000/files`. Its installer is explicitly named
`SynthiusDendrite-local-dev-setup.exe`; keep it separate from public releases.
The helper uses debug outputs, its own app identity and `SynthiusDendriteLocalDev`
storage, and disables automatic updating. It shares port 1543 with production,
so stop that instance before running the local build. Production/preview release
builds cannot target the local project.

The window uses the original black-and-white Synthius vector logo with tracing
edge fragments removed. Windows loads multi-size ICO assets natively, with
separate small title-bar and larger taskbar icons. Tray icons use transparent
monochrome glyphs (white on a dark taskbar, dark on a light taskbar), selected
for the display scale. DPI/theme changes refresh these icons.

The small footer stays pinned to the bottom edge and always shows the installed
version. Dendrite checks for a newer release after startup and every six hours,
shows **Latest version** when current, and offers **Update** only for a newer
signed release. Update connectivity never blocks local source access; failures
stay silent and are handled by the automatic schedule. Updater SDK diagnostics
are filtered from desktop logs; application and security logging remain active.

Regenerate the desktop assets with `npm install` followed by
`node scripts/build-icons.mjs` from this repository root.
The generator supersamples the existing vector artwork, keeps alpha
antialiasing, and writes exact-size PNGs and multi-size Windows ICO files.

### Run tests locally

```powershell
npm test
cargo test --workspace --locked
npm run test:desktop
```

These public checks use synthetic fixtures and mocks, without Telegram
authorization or paid model calls. Maintainers separately verify the private
host integration. Contributors do not need that private checkout. A
Telegram-enabled build additionally requires separately authorized live and
packaged-platform checks after the provider-policy decision.

See [CONTRIBUTING.md](CONTRIBUTING.md) to add a reviewed format adapter and
[RELEASING.md](RELEASING.md) for the clean-repository and parser-artifact
release boundary.

### Build a Synthius Dendrite release locally

```powershell
npm ci
npm run build:production
```

The build command automatically loads the checked-in public Production key from
`distribution/public-client/production.json`. Developers need no dashboard access,
manual key exchange, Supabase instance or private webapp source. Rust receives
the selected key from its build environment and contains no hardcoded fallback.
Self-built clients may connect to Production using the public Production key
and the user's own Synthius account. The key is public client configuration,
included in this repository, not a maintainer-only secret.
This does not grant another user's data, repository write access or official
release authority. Account-backed features must match the selected backend's
supported API/protocol; new Preview features are not automatically deployed to
Production. The backend treats every client's requests and parsed corpora as
untrusted, regardless of who built the executable.

Preview is maintainer-only. Its endpoints and key are not part of the public
configuration. Maintainers supply an external `SYNTHIUS_PREVIEW_CONFIG_FILE`
when using the Preview builder, as documented in [RELEASING.md](RELEASING.md). Authentication values are
compile-time values so an arbitrary browser page cannot redirect JWT validation
to an attacker-controlled project. Never embed a service-role or secret key.

The **Open Synthius** link follows the compiled authentication environment:
production → `app.synthius.ai`, maintainer preview → its privately configured webapp, and the
isolated local project → `localhost:3000`. It opens **Sources** in the default
browser. Dendrite's window stays on its local status page; arbitrary
external navigation is blocked. **Local service ready** describes the desktop
service, while the web app verifies its own connection and each source action.

`cargo audit` currently reports `RUSTSEC-2023-0071` through
`jsonwebtoken -> rsa`. The advisory concerns timing leakage from RSA private-key
operations. This sidecar owns no RSA private key and uses the crate only to
verify Supabase signatures with public JWKS material, so the vulnerable
operation is not reachable. Keep the exception visible and remove it when the
upstream RustCrypto replacement becomes available.

### Use the repo helper

```powershell
.\build_all.ps1
```

## Frontend dev targeting

Use `NEXT_PUBLIC_API_URL` for the online Synthius backend. Dendrite always stays
on authenticated loopback endpoints. Source residence no longer changes the
backend target: a mixed-source pipeline stages its local inputs and runs through
the same backend as its cloud inputs.

## CI packaging

GitHub Actions builds the thin runtime for three release targets:

- Windows x64 NSIS installer
- macOS Apple Silicon DMG
- macOS Intel DMG

The workflow packages the Tauri app directly and does not build or bundle a local Python backend.
It uses Node.js 24, pinned Rust 1.92.0, and Tauri CLI 2.12.0.

## Troubleshooting

### Connect local source search

Pair Dendrite with your signed-in Synthius account once. The owner/device-bound
search capability is saved in Windows Credential Manager or macOS Keychain and
restored when Dendrite starts. Older paired installations obtain it through
their existing native account grant. Expired or revoked grants require another
account connection. Backend/network interruptions reconnect automatically with
bounded backoff. Telegram, schedules and MCP can use that connection while the
browser is closed. An offline local scope returns a typed warning while cloud
and structured persona-memory results continue.

The backend's private Realtime topic policy must be applied before hosted
rollout. Dendrite receives neither a service-role key nor provider credentials.
Relay requests and results are ephemeral, purpose-encrypted between API workers,
and scoped to exact user, persona, source and device. Local artifact SHA-256,
size and embedded identity are checked before every read.

### Synthius Dendrite is running but the web app cannot connect

1. Check the tray or menu bar icon.
2. Open the logs folder from the tray menu and inspect `sidecar.log`.
3. Confirm the local runtime answers on `http://127.0.0.1:1543/api/health`.

### A local move has not finished

1. Make sure the source's owning Dendrite is running and the web app is online.
2. Open Sources and select the move action again to resume.
3. Check `sidecar.log` and the web app console for verification failures.

### Local files do not appear

1. Open the storage folder from the tray menu.
2. Confirm files are being created under `uploads/` or `outputs/`.
3. Check `sidecar.log` for failed auth or filesystem access errors.

## License

Apache-2.0
