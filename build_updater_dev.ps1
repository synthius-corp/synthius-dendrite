param(
    [string]$PrivateKeyPath = "..\work\dendrite-updater-dev\dendrite.key",
    [string]$ArtifactBaseUrl = "https://updates.invalid/dendrite/windows-x86_64",
    [string]$PrivateKeyPassword = ""
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$key = [System.IO.Path]::GetFullPath((Join-Path $root $PrivateKeyPath))
if (-not (Test-Path -LiteralPath $key -PathType Leaf)) {
    throw "Updater private key not found: $key"
}
$publicKeyPath = "$key.pub"
if (-not (Test-Path -LiteralPath $publicKeyPath -PathType Leaf)) {
    throw "Updater public key not found: $publicKeyPath"
}
$configPath = Join-Path $root 'src-tauri\tauri.conf.json'
$config = Get-Content $configPath -Raw | ConvertFrom-Json
$configuredPublicKey = [string]$config.plugins.updater.pubkey
$buildPublicKey = (Get-Content -LiteralPath $publicKeyPath -Raw).Trim()
if ($configuredPublicKey -ne $buildPublicKey) {
    throw 'The signing key does not match the updater public key compiled into Dendrite.'
}

$tauri = Join-Path $root 'node_modules\.bin\tauri.cmd'
if (-not (Test-Path -LiteralPath $tauri -PathType Leaf)) {
    throw 'Run npm install in local-agent before building updater artifacts.'
}

$previousKey = $env:TAURI_SIGNING_PRIVATE_KEY
$previousPassword = $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD
try {
    $env:TAURI_SIGNING_PRIVATE_KEY = $key
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $PrivateKeyPassword
    Push-Location (Join-Path $root 'src-tauri')
    try {
        & $tauri build --ci --config tauri.updater.conf.json --bundles nsis -- --locked
        if ($LASTEXITCODE -ne 0) { throw 'Signed Dendrite updater build failed' }
    } finally {
        Pop-Location
    }
} finally {
    $env:TAURI_SIGNING_PRIVATE_KEY = $previousKey
    $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = $previousPassword
}

$version = [string]$config.version
$bundle = Join-Path $root 'src-tauri\target\release\bundle\nsis'
$installer = Get-ChildItem -LiteralPath $bundle -File -Filter '*.exe' |
    Where-Object { Test-Path -LiteralPath "$($_.FullName).sig" } |
    Sort-Object LastWriteTimeUtc -Descending |
    Select-Object -First 1
if (-not $installer) { throw 'The signed NSIS installer and its .sig file were not produced.' }

$out = Join-Path $root 'dist\updater-dev\windows-x86_64'
New-Item -ItemType Directory -Path $out -Force | Out-Null
$artifactName = "SynthiusDendrite_${version}_windows_x86_64.exe"
$artifact = Join-Path $out $artifactName
Copy-Item -LiteralPath $installer.FullName -Destination $artifact -Force
Copy-Item -LiteralPath "$($installer.FullName).sig" -Destination "$artifact.sig" -Force

$signature = (Get-Content -LiteralPath "$artifact.sig" -Raw).Trim()
$manifest = [ordered]@{
    version = $version
    notes = 'Synthius Dendrite development updater build'
    pub_date = [DateTimeOffset]::UtcNow.ToString('o')
    platforms = [ordered]@{
        'windows-x86_64' = [ordered]@{
            url = "$($ArtifactBaseUrl.TrimEnd('/'))/$artifactName"
            signature = $signature
        }
    }
}
$manifestPath = Join-Path $out 'latest.json'
$manifestJson = ($manifest | ConvertTo-Json -Depth 5) + [Environment]::NewLine
$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllText($manifestPath, $manifestJson, $utf8NoBom)

Write-Host "Signed updater artifact: $artifact"
Write-Host "Updater manifest: $manifestPath"
if ($ArtifactBaseUrl -like 'https://updates.invalid/*') {
    Write-Warning 'The default URL is intentionally non-routable. Publish the files to a private HTTPS development location before an end-to-end install test.'
}
