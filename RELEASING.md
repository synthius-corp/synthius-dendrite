# Dendrite release boundary

The canonical public source is `synthius-corp/synthius-dendrite`. It contains
reviewed Dendrite source and synthetic fixtures. Earlier source history, releases,
tags and run metadata remain in the private `synthius-dendrite-private-archive`.
Do not import that history or relabel old artifact provenance. Private Preview
configuration, Synthius application history, secrets and user data are excluded.
Only the owner accepts upstream changes and starts official distribution.

The Linux parser executable is a build product of this repository, not a second
service or repository. The Synthius backend image consumes a pinned official
release and installs the executable/checksum pair at `/app/bin`. Its parser
path can be preset by the image; desktop/local-dev paths are platform-specific.
Private cross-repository read access belongs to the maintainer's release-refresh
step only. Hosted builds consume the already verified vendored artifacts.

The main application keeps a pinned development submodule and vendors the
reviewed Linux parser archive and `@synthius/dendrite-source-selection` tarball
from an official release. Version, source commit and SHA-256 are recorded in
the application's release lock. Builds need no private GitHub credential;
release refresh uses an authorized maintainer's GitHub access.

## Production releases through GitHub Actions

GitHub Actions builds only Production distributions. Preview builds are local
and follow the separate procedure below.

1. Merge reviewed changes and inspect existing CI evidence.
2. Set the desktop version in `src-tauri/Cargo.toml`, its lockfile and
   `src-tauri/tauri.conf.json`; bump parser/UI versions when their contracts change.
   Refresh distribution notices with `npm run licenses` after lockfile changes;
   `npm test` checks their lock/source receipt before release validation.
3. Configure and verify the main/tag protections and **owner-release** approval
   environment in [owner controls](docs/OWNER_CONTROLS.md). Only @x-aero may
   dispatch or rerun distribution jobs. Configure `SYNTHIUS_TELEGRAM_API_ID`
   and `SYNTHIUS_TELEGRAM_API_HASH` in that protected environment. Set the
   checked-in `distribution/public-client/production.json` to the Production
   project's public key. Official and community builders load the same explicit
   configuration and pass it as `SYNTHIUS_SUPABASE_PUBLIC_KEY`; Rust has no key fallback.
4. As @x-aero, create the exact stable tag at the reviewed main commit, then
   manually dispatch `Release Dendrite (Production)` from **main**
   with the exact stable tag matching the desktop version, such as `v2.1.4`.
   The workflow validates CI, builds all four targets, creates checksums,
   CycloneDX inventories and provenance, verifies the complete artifact set,
   then publishes an immutable GitHub Release. Tag pushes never build a release.
5. Refresh the consuming application's pinned Production release and run its
   integration and packaged-image checks before Production promotion.

```sh
git push origin REVIEWED_MAIN_COMMIT:refs/tags/v2.1.4
gh workflow run package.yml --ref main -f release_tag=v2.1.4
```

The desktop-only Production workflow avoids rebuilding unchanged parser/UI
artifacts. Both hosted release workflows have a fixed `production` channel and
reject Preview tags. Production publication emits `manifest-production.json`
and makes the stable release latest.

## Local Preview releases

Preview installers are built on trusted local machines for their native target.
They are published only to `synthius-corp/synthius-dendrite-private-archive`;
the publisher checks its immutable ID and private visibility before uploading.
No Preview binaries or their embedded private environment settings are published
in the public repository. Production releases use the canonical public repository.
Before a new Preview build, the maintainer makes its reviewed public source commit
available under the exact Preview tag in the private repository, for example:

```sh
git remote add private-archive https://github.com/synthius-corp/synthius-dendrite-private-archive.git
git push private-archive SOURCE_COMMIT:refs/tags/vX.Y.Z-preview.N
```

Use the exact reviewed commit/tag, not a branch alias. This imports public source
into the private archive; never push archive history to the public repository.
Then follow the native build commands:
Windows x64 on Windows, Apple Silicon on an Apple Silicon Mac, and Intel on an
Intel Mac. GitHub Actions is not used. Each machine checks out the same reviewed
commit and uses the same unused `vX.Y.Z-preview.N` tag. The Preview Supabase
public key and Telegram application identity stay in the local build environment;
they are never written to source, build logs or release metadata.

Preview configuration is private to maintainers. Store `supabaseUrl`,
`supabasePublicKey`, `webAppUrl` and `backendUrl` in a JSON file outside the
repository and select it with `SYNTHIUS_PREVIEW_CONFIG_FILE`. The builder rejects
an in-repository file. Do not commit the real Preview endpoints/key or copy them
into the public contribution history.

Set `SYNTHIUS_RELEASE_CHANNEL=preview`, `DENDRITE_RELEASE_TAG`,
`SYNTHIUS_PREVIEW_CONFIG_FILE`, `SYNTHIUS_TELEGRAM_API_ID`,
`SYNTHIUS_TELEGRAM_API_HASH` and `GITHUB_SHA` to the exact checked-out commit.
Run `node scripts/desktop.cjs test`, `node scripts/desktop.cjs build`, then set
`DENDRITE_RELEASE_TARGET` to the native target and run
`node scripts/desktop.cjs collect`. Preserve that machine's clean `staging/`
directory. To combine targets, copy each complete artifact/checksum pair into
one clean staging directory without replacing files.

Publish the verified set from the maintainer's authenticated machine:

```sh
node scripts/desktop.cjs publish-local-preview STAGING_DIRECTORY v2.1.4-preview.N EXACT_SOURCE_COMMIT
```

The publisher accepts one or more locally built desktop targets, verifies every
adjacent checksum and provenance record, creates a GitHub prerelease, and emits
`manifest-preview.json` containing only platforms actually present. Published
releases are immutable; add another platform through a new Preview version, not
by modifying an existing release. The Synthius Preview frontend pins that exact
manifest. Never publish real Telegram account content or login material.

## License and notice packaging

`distribution/licenses/` is a checked-in, versioned notice bundle. It includes
the application license, original dependency notices/copyright headers,
declared authors, selected license terms and exact-version source links. Its
inventory conservatively includes Cargo metadata across targets and optional,
build and development dependencies; it does not claim all entries are linked
into every binary. Standard texts come from the pinned SPDX source recorded in
`distribution/license-texts/SOURCE.json`.

`npm run licenses` refreshes the bundle from downloaded locked Cargo packages
without compiling. If a new dependency needs a new standard text, add that text
from SPDX first. `npm test` rejects stale locks or changed notice files.
Parser archives include the `licenses/` directory, and Tauri resources map it
into each installed desktop app. The dependency-free UI package includes its
own Apache license. SBOMs retain dependency license expressions.

For listed MPL components, the notice links to their exact unmodified crate
source archives and retains MPL terms. Changes to those components require
making the modified corresponding source available too. Dependencies retain
their own licenses; the application license does not override them.
See [MPL distribution requirements](https://www.mozilla.org/en-US/MPL/2.0/).

## Publication recovery and compute discipline

`Dendrite CI` tests PR suggestions and owner-requested main validation; main
pushes do not automatically repeat it. `Release Dendrite (Production)` validates once,
packages platform artifacts and publishes an exact tag. These are
different responsibilities, not two installer builders.

Both official workflows accept optional `validation_run_id`. Supply a completed
successful full `Dendrite CI` run for the exact main source to skip duplicate
validation. Its four native platform jobs and final acceptance must pass; a
policy-only smoke run cannot be reused. Without that input, required native tests
run before building. Owner/main/environment and artifact checks always apply.
See [paid CI policy](docs/OWNER_CONTROLS.md#paid-ci-policy) for PR cancellation,
documentation-only planning, timeouts and external-run approval requirements.

For a desktop-only Production release, manually dispatch `Release Dendrite
Desktop (Production)` from main with the matching stable version tag.
It runs full parser/worker acceptance checks as well as desktop tests and builds
only the three affected installer targets. It reuses existing parser/UI releases.
The owner-release environment gates preflight, credential-bearing build and
publication, and must have @x-aero as its only reviewer. An absent/unprotected
main or unconfigured approval environment makes preflight fail.

Inspect failure logs first. If platform builds succeeded and only publication
failed, reuse their original artifacts after checksum/provenance verification.
Retry only the failed publish job for a transient failure. A rerun retains the
original commit and workflow, so a publisher fix needs a corrected publisher
using the retained artifacts, not a blind rerun or another full release build.
Preserve the original tag and artifact source commit; published versions stay
immutable. Resume an unpublished draft only after validating its existing
assets. Do not bump a version, recreate installers, or repeat passed platform
tests solely because upload/publication failed. Rebuild only changed build
inputs, failed integrity checks or unavailable artifacts. Avoid redundant
main-push CI and tag validation for the same commit. Record the reason before
starting additional billable Actions work.

Recover a full release's publication with its original complete artifact set:
`node scripts/release.cjs publish-existing STAGING_DIRECTORY production TAG SOURCE_COMMIT`.
This uses the same channel/profile/provenance checks and resumes only an exact
draft; it performs no compiler/build invocation.

The initial installers use unsigned Windows/ad-hoc macOS packaging. Their
automatic update endpoints and public key are cleared by
`tauri.release.conf.json`. This keeps the development updater trust root out of
distributed installers. Enable production updates only after configuring the
permanent key and signed publication workflow described below.

## Required release artifacts

### Environment-specific desktop builds

`distribution/profiles.json` is the shared native/build profile registry.
Production preserves `app.synthius.sidecar`, port 1543 and the existing
`SynthiusSidecar` data directory. Preview uses `app.synthius.dendrite.preview`,
port 1544 and `SynthiusDendritePreview`; its app name includes Preview. Each
build pins its own Supabase Auth project, web app and API. JWT issuer and CORS
checks reject the other hosted environment. Native upload credentials remain
scoped to project and device.

Use `Release Dendrite Desktop (Production)` (`desktop.yml`) for Production
desktop-only changes:

```sh
gh workflow run desktop.yml --ref main -f release_tag=v2.1.4
```

The hosted workflow has a fixed Production profile and uses the Telegram
application identity from the protected release environment. Local Preview
builds require the matching Preview public anon/publishable key and reject
privileged keys or keys from another Auth project. Preview uses
`vX.Y.Z-preview.N`; the native installer base version remains `X.Y.Z` for
platform compatibility.

This workflow tests and builds Windows x64, Apple Silicon and Intel only. It
publishes installers, SBOMs, checksum/provenance receipts and the Production
website manifest, retaining exact source commit and tag. It does not rebuild or
replace the previously verified Linux parser or source-selection package. The
main webapp validates the manifest channel and selects preview port 1544 from
its existing Supabase URL. macOS launch commands use the matching app name.

The Production preflight blocks a second build of an existing release/draft. If only
publication fails, use the original workflow artifacts with the corrected
publisher; do not repeat successful platform builds. Automatic updater endpoints
and the development trust key are cleared in every distributed desktop profile.

Preflight reserves the exact source tag before compiling. This retains the
build's source reference if `main` advances during a long build.
An existing tag must resolve to the exact build source;
preflight never moves it. A preflight permission failure stops before billable
platform builds. Publication still happens only after every artifact is verified.

Finish workflow maintenance before dispatch and keep `.github/workflows/`
unchanged until publication. GitHub can reject the Actions token when a historical
release target has a different workflow tree from the default branch, even with
an existing tag. Do not keep rerunning that publisher or rebuild installers.
Use the already-authorized maintainer's local GitHub CLI login with workflow
permission and the retained Production artifact directory:

```sh
node scripts/desktop.cjs publish-existing /path/to/verified-artifacts production v2.1.4 EXACT_ORIGINAL_BUILD_COMMIT
```

This verifies every installer/profile/source/checksum again, creates the matching
manifest and publishes only those original artifacts. It can resume an incomplete
draft after checking its exact source, channel and existing asset digests, then
uploads only missing assets. A conflicting draft or published release is rejected;
no release assets are overwritten. No new GitHub Actions run is required.

A failed artifact upload gets one upload-only retry in the same job, reusing
its staged installer and receipts. Rust build caches are retained on failure.
If both uploads fail and the hosted runner is discarded, its unsaved installer
is unavailable: rerun only that affected platform, retaining the other platforms'
original artifacts and source commit.

1. Build and test the locked parser workspace and the Tauri host.
2. Publish the platform desktop installers.
3. Publish the standalone `dendrite-parser` executable/checksum archive for each
   supported platform, and the versioned source-selection npm tarball.
4. Publish SHA-256 checksums, lockfile fingerprints, provenance and an SBOM;
   exact dependency lockfiles remain in the tagged source.
5. Package the exact cloud parser executable with its generated adjacent
   `dendrite-parser.sha256`, then set `DENDRITE_PARSER_BIN`. Verification is
   automatic and requires the adjacent file; there is no checksum environment
   variable or override to maintain.
6. Keep automatic updates disabled for the initial unsigned/ad-hoc release.
   The future production updater channel follows the separate trust boundary
   below; it is not published by the current release workflow.

The repository workflow emits reproducible package candidates and SHA-256
files. Production publication must apply the available platform signing and
notarization credentials outside contributor code. Until those credentials are
configured, release artifacts must be described as unsigned; packaging remains
available for development and compatibility testing.

macOS packaging uses the ad-hoc identity `-` by default for Apple Silicon
compatibility. It requires no paid Apple certificate. The installation guide
shows the user how to remove only the installed Dendrite app's quarantine flag
and launch it. This is separate from the mandatory
cryptographic signatures used by the in-app updater. The private installer
workflow also requires `SYNTHIUS_TELEGRAM_API_ID` and
`SYNTHIUS_TELEGRAM_API_HASH` Actions secrets for snapshot-enabled releases.
No credential values are committed or configured remotely by these references.

The private service never downloads adapters at runtime. Community changes
reach cloud and desktop ingestion only through one reviewed, versioned Dendrite
release. The cloud worker checks the executable digest and capability catalogue
before parsing; the parser subprocess receives no application credentials.

## Compatibility

Corpus compatibility is the deployment boundary. Desktop and cloud parser
versions may advance independently only while both emit a supported
`synthius-corpus/v1` contract. A parser change that changes ordered JSONL bytes
must change its package version and provide deterministic golden fixtures.

## Desktop update trust boundary

The committed updater key is currently a development trust root. Before the
first production updater release, generate and durably back up a production
Tauri updater keypair, replace the committed public key, and store the private
key and password only in the release environment. Losing that private key
prevents future in-app updates; exposing it permits malicious signed updates.

The current release workflow builds with `tauri.release.conf.json`, clearing
the development public key and all updater endpoints. It publishes versioned
installers, not updater manifests. A future updater publication workflow must
require a permanent production public key and inject it into every eligible
build; the development trust root must never enter that channel.

Production release CI must set `TAURI_SIGNING_PRIVATE_KEY` (or its documented
path form) and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, then build with
`--config tauri.updater.conf.json`. The resulting `.sig` content is embedded in
the target manifest. Dendrite accepts HTTPS endpoints only, rejects downgrades,
and requires the trusted signature comment to carry the same version advertised
by the manifest.
