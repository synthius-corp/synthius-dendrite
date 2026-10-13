# Synthius Dendrite Development

## Quick start

Clone this standalone repository and work from its root (`synthius-dendrite`
by default). Install Rust 1.92.0, Node.js 24, and the platform Tauri prerequisites.
Windows desktop builds need MSVC Build Tools, Windows SDK and WebView2;
macOS needs Xcode command-line tools; Linux desktop tests need the packages
listed in `.github/workflows/ci.yml`. `npm ci` installs the pinned Tauri CLI;
use its Node entry point rather than an independently installed Cargo CLI.
Parser-only tests need no desktop installation, Docker or service credentials.

Run `npm test`, workspace formatting/Clippy and `cargo test --workspace --locked`
first. See [CONTRIBUTING](CONTRIBUTING.md) and the [worked adapter example](docs/ADDING_FORMAT.md).

Standalone desktop tests and debug builds need no Supabase, Docker, Synthius
account or private app checkout. Use `npm run test:desktop`,
`npm run build:offline` or `npm run dev:offline`; the helper supplies an explicit
nonfunctional public-key fixture and the isolated local-dev identity. Builds
may download ordinary Rust/npm dependencies. Account-backed features remain
unavailable without live integration configuration; authentication is not bypassed.

For maintainer integration with a fully local Windows webapp, use `build_local_dev.ps1` with the
isolated Supabase URL on loopback port 54321 and its public key. It uses a debug
build, separate identity/storage and disabled updates; outputs are under
`dist/local-dev`. Production and preview release profiles cannot target this
local project. No app needs to be launched to run the offline acceptance tests.

### Run in development

```powershell
npm ci
npm run dev:offline
```

This launches the tray app and starts the localhost thin runtime on port `1543`.

### Run the Rust tests

```powershell
cargo test --workspace --locked
npm run test:desktop
```

### Build a release locally

```powershell
npm ci
npm run build:production
```

This ordinary build does not create updater artifacts. For a signed development
candidate, keep the private key under the root ignored `work/` directory and
run `./build_updater_dev.ps1`. Do not commit private keys or generated artifacts.

### Use the repo helper

```powershell
.\build_all.ps1
```

## Architecture notes

- Synthius Dendrite does not spawn the Python backend locally.
- The localhost service is implemented directly in Rust.
- The local runtime handles health, owner-scoped filesystem storage,
  deterministic classification/parsing, and local source-index persistence.
- Dendrite handles deterministic parsing of local sources. Online APIs handle
  validated-corpus processing, extraction, consolidation, pipelines, and metadata.
- Local source/corpus/index bundles move together under a verified per-source transaction.

## Dev backend targeting

`NEXT_PUBLIC_API_URL` selects the online backend for every source residence.
Dendrite remains on `127.0.0.1:1543`. A mixed-source pipeline prepares local
inputs before it creates a run on that same backend.

## Manual checks

- Tray icon appears and shows `Local Runtime: Running`.
- Repeated launch leaves one desktop process and activates its existing
  window. Repeat with that window minimized and hidden in the tray. Keep a
  Telegram worker running when available: its worker CLI bypasses the desktop
  single-instance guard.
- `http://127.0.0.1:1543/api/health` returns a healthy response.
- The web app can authenticate and synchronize the metadata-only source catalogue.
- **Open Synthius** opens Sources in the default browser and leaves
  the Dendrite window open. The link follows the compiled authentication project.
- The status changes to **Local service ready** after health succeeds. Closing
  the window leaves the service available; tray **Quit** stops it.
- The small footer remains pinned to the bottom edge and shows the app version.
  A successful no-update check shows **Latest version**; endpoint/network
  failures remain silent and the next automatic check occurs within six hours.
- Files added through Dendrite appear under the local storage root; ordinary
  web uploads remain cloud sources.
- Cloud and Local rows appear distinctly in both source catalogues.
- Overview, Sources and Telegram tabs work with mouse and keyboard (vertical
  arrows, Home and End). All/Local/Cloud filters survive catalogue refreshes.
  Sources starts with Local selected. Its top-right Add opens the same local
  upload flow as Overview. A Local row's eye opens the exact source's parent
  folder; Cloud rows have no eye. Check missing files and old cached records
  before catalogue resynchronization: neither opens an unrelated folder.
  Check the default 920×680 and minimum 760×560 window sizes: the version footer
  stays visible and the Sources list has its own scroll area.
- Moving one source transfers and verifies its source, corpus and index before
  cutover; repeating the action resumes an interrupted move.

## Logs

Open the logs folder from the tray or menu-bar action.

`sidecar.log` contains local runtime startup and request diagnostics.

## Useful commands

```powershell
# Test the runtime health endpoint
curl http://127.0.0.1:1543/api/health

# Build the Synthius Dendrite installer bundle
node node_modules/@tauri-apps/cli/tauri.js build --ci --config src-tauri/tauri.release.conf.json -- --locked
```

## GitHub Actions packaging

The release workflow packages Synthius Dendrite for:

- Windows x64
- macOS Apple Silicon
- macOS Intel

It no longer installs backend requirements or invokes PyInstaller.

## Packaging note

The legacy Python-runtime build path has been removed. Use the pinned Node Tauri CLI or `./build_all.ps1` for local desktop packaging.
Official releases use the owner-approved workflows in [RELEASING](RELEASING.md).
`npm run build:production` loads public Production configuration from this
repository and builds an executable without an installer. Maintainer Preview
configuration stays in an external file selected by `SYNTHIUS_PREVIEW_CONFIG_FILE`.
Neither channel has a hardcoded Rust key fallback.
