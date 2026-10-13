# Building Synthius Dendrite Distributives

## Repository packages (GitHub Actions)

```powershell
# Owner only: review the exact main commit and configured protections first
gh workflow run package.yml --ref main -f release_tag=v2.1.4

# Monitor
gh run watch
```

**Outputs:**
- Windows installer
- macOS Apple Silicon image
- macOS Intel image
- Linux, Windows and macOS pinned parser executables
- SHA-256 sidecar files for every artifact

Hosted full and desktop-only releases build Production only. Preview builds
are local; see [RELEASING.md](RELEASING.md) for their verified publication flow.
Preview uses its separate app/storage/port and backend, a prerelease tag and
`manifest-preview.json`; production uses its existing identity, a stable tag and
`manifest-production.json`. Provenance verification rejects mixed channels.
Preview releases are never marked as GitHub's latest production release.

Production builds automatically load the checked-in public client configuration;
community developers use `npm run build:production` without receiving a key from
a maintainer. Preview is maintainer-only and loads an external private
`SYNTHIUS_PREVIEW_CONFIG_FILE`; no real Preview configuration is tracked.
Rust has no hardcoded public-key fallback. For standalone contributors,
`npm run build:offline` builds a debug executable without an installer, using
a nonfunctional key and local identity. It needs no private app, Supabase
instance or Docker. `npm run test:desktop` runs the native tests the same way.

The desktop workflow packages only the Tauri app. It does not build or bundle a local Python backend, and it no longer installs or bundles `ffmpeg`/`ffprobe`.

Telegram import is compiled into this same Rust executable. The child worker
uses an internal entry point, so there is no extra helper/runtime artifact.
For a Telegram-enabled development build, supply `SYNTHIUS_TELEGRAM_API_ID`
and `SYNTHIUS_TELEGRAM_API_HASH` as build environment values. The importer is
unavailable in a credential-free development build. Official workflows supply
these secrets from the owner-release environment and include Telegram import.
The official builder rejects missing/invalid identities. There is no feature
toggle or policy-approval variable. Referencing
them in the workflow does not create or upload secrets. Keep application
credentials outside Git. The native
UI and memory-only worker ship with the ordinary Tauri assets.

macOS is built separately on Apple Silicon and Intel runners. The default
`bundle.macOS.signingIdentity` is `-` for free ad-hoc code signing, required
for the unsigned Apple Silicon distribution path. This provides a code
integrity signature, not an Apple-verified developer identity or notarization.
The web download guide and wiki show one Mac first-launch procedure: install
in Applications, remove only Dendrite's `com.apple.quarantine` attribute with
`xattr -r -d`, then launch the quoted app path with `open`. Their command path
must match Tauri's `productName`. This per-app quarantine bypass is intended
only for the trusted Synthius download; persistent errors and malware warnings
require support. Windows shows the SmartScreen first-launch procedure.

**Updates:** Dendrite checks after startup and every six hours, and shows its
version and update state in the small bottom-pinned footer. A newer signed
release exposes **Update**; current installs show **Latest version**. Check and
download failures are non-blocking, stay out of the UI and rely on the automatic
schedule.

Updater publication remains development-only until the permanent production
signing key is stored in the release secret manager. Ordinary builds keep
`createUpdaterArtifacts` disabled and need no private key. Build a signed
development candidate with:

```powershell
npm ci
.\build_updater_dev.ps1
```

The helper verifies that the ignored private key matches the public key compiled
into Dendrite, enables `tauri.updater.conf.json`, and writes the signed NSIS
artifact and `latest.json` under `dist/updater-dev/windows-x86_64/`. Its default
artifact URL is intentionally non-routable; publish the files to a private HTTPS
development location for an end-to-end install test.

---

## Development (Local)

```powershell
.\build_all.ps1           # Windows only, increments version
.\build_all.ps1 -NoIncrement  # Skip version bump
```

Output: versioned installers and SHA-256 files under `dist/releases/`

For an isolated Windows build without changing versions or the web download manifest:

```powershell
npm ci
.\build_local_dev.ps1  # isolated loopback URL/public key must be set
```

The isolated debug installer is copied to `dist/local-dev/` with its own identity
and storage. It never enables the production updater. Local builds are
currently unsigned and should be treated as development/test artifacts. This
command publishes nothing. The release helper refuses a failed build or a
missing matching-version installer, and records the size and SHA-256 only for
platform artifacts it actually copies.

Windows verification on 2026-09-26 built the NSIS installer, installed it in an
isolated project directory, checked the installed executable against the build
hash, and visually inspected the installed window. The web-app button opened
Storage settings in the default browser, which detected the running service.
Closing the desktop window preserved service availability and the existing
vault/device identity. No mode migration, provider call or hosted database
change was performed.

Updater artifacts use Tauri signatures. `requireSignedVersion` binds the
announced version to the artifact signature, downgrades and insecure transport
are disabled, and Windows uses passive current-user installation. The private
key must never be committed or copied into the app; only its public half belongs
in `tauri.conf.json`.

---

## Quick Commands

| Action | Command |
|--------|---------|
| Publish reviewed release | Owner dispatches `package.yml` from protected main with the exact version tag |
| View releases | `gh release list` |
| Local build | `.\build_all.ps1` |
