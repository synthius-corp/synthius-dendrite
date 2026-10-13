# Build and package Synthius Dendrite without relying on another repository.
param(
    [switch]$Minor,
    [switch]$Major,
    [switch]$NoIncrement
)
$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$tauriConfig = Join-Path $root 'src-tauri/tauri.conf.json'
$cargoToml = Join-Path $root 'src-tauri/Cargo.toml'
$dist = Join-Path $root 'dist/releases'

$config = Get-Content -LiteralPath $tauriConfig -Raw | ConvertFrom-Json
$version = [Version]$config.version
if (-not $NoIncrement) {
    if ($Major) {
        $version = [Version]::new($version.Major + 1, 0, 0)
    } elseif ($Minor) {
        $version = [Version]::new($version.Major, $version.Minor + 1, 0)
    } else {
        $version = [Version]::new($version.Major, $version.Minor, $version.Build + 1)
    }
    $versionText = $version.ToString(3)
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    $tauriText = (Get-Content -LiteralPath $tauriConfig -Raw) -replace '"version"\s*:\s*"[0-9]+\.[0-9]+\.[0-9]+"', "`"version`": `"$versionText`""
    [IO.File]::WriteAllText($tauriConfig, $tauriText, $utf8)
    $cargoText = Get-Content -LiteralPath $cargoToml -Raw
    $packageStart = $cargoText.IndexOf('[package]')
    $versionStart = $cargoText.IndexOf('version =', $packageStart)
    if ($packageStart -lt 0 -or $versionStart -lt 0) { throw 'Tauri package version is unavailable' }
    $lineEnd = $cargoText.IndexOf("`n", $versionStart)
    if ($lineEnd -lt 0) { $lineEnd = $cargoText.Length }
    $cargoText = $cargoText.Substring(0, $versionStart) + "version = `"$versionText`"" + $cargoText.Substring($lineEnd)
    [IO.File]::WriteAllText($cargoToml, $cargoText, $utf8)
} else {
    $versionText = $version.ToString(3)
}

Push-Location $root
try {
    & cargo metadata --manifest-path src-tauri/Cargo.toml --offline --format-version 1 --no-deps | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Desktop version lock refresh failed' }
    & cargo fmt --all --check
    if ($LASTEXITCODE -ne 0) { throw 'Parser formatting check failed' }
    & cargo clippy --workspace --all-targets --locked -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Parser lint failed' }
    & cargo test --workspace --locked
    if ($LASTEXITCODE -ne 0) { throw 'Parser tests failed' }

    Push-Location (Join-Path $root 'src-tauri')
    try {
        & cargo fmt --all --check
        if ($LASTEXITCODE -ne 0) { throw 'Desktop formatting check failed' }
        & cargo clippy --all-targets --locked -- -D warnings
        if ($LASTEXITCODE -ne 0) { throw 'Desktop lint failed' }
        & cargo test --locked
        if ($LASTEXITCODE -ne 0) { throw 'Desktop tests failed' }
        & node (Join-Path $root 'node_modules/@tauri-apps/cli/tauri.js') build --ci --config tauri.release.conf.json -- --locked
        if ($LASTEXITCODE -ne 0) { throw 'Dendrite package build failed' }
    } finally {
        Pop-Location
    }

    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    $artifacts = @(
        Get-ChildItem -Path (Join-Path $root 'src-tauri/target/release/bundle') -File -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.Extension -in @('.exe', '.dmg', '.deb', '.AppImage') }
    )
    if ($artifacts.Count -eq 0) { throw 'No installer artifact was produced' }
    foreach ($artifact in $artifacts) {
        $target = Join-Path $dist $artifact.Name
        Copy-Item -LiteralPath $artifact.FullName -Destination $target -Force
        $digest = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
        Set-Content -LiteralPath "$target.sha256" -Value "$digest  $($artifact.Name)" -Encoding ascii
    }
    Write-Host "Dendrite $versionText artifacts: $dist"
} finally {
    Pop-Location
}
