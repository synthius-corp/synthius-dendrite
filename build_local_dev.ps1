# Isolated Windows development build. Install the locked npm tools first.
param(
    [string]$SupabaseUrl = $env:SYNTHIUS_SUPABASE_URL,
    [string]$SupabasePublicKey = $env:SYNTHIUS_SUPABASE_PUBLIC_KEY,
    [switch]$NoBundle
)
$ErrorActionPreference = 'Stop'
$names = @('CARGO_TARGET_DIR', 'SYNTHIUS_RELEASE_CHANNEL', 'SYNTHIUS_SUPABASE_URL', 'SYNTHIUS_SUPABASE_PUBLIC_KEY', 'TAURI_CONFIG')
$previous = @{}
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
Push-Location (Join-Path $PSScriptRoot 'src-tauri')
try {
    $env:SYNTHIUS_SUPABASE_URL = $SupabaseUrl
    $env:SYNTHIUS_SUPABASE_PUBLIC_KEY = $SupabasePublicKey
    $planArguments = @((Join-Path $PSScriptRoot 'scripts/local-dev.cjs'))
    if ($NoBundle) { $planArguments += '--no-bundle' }
    $planJson = & node @planArguments
    if ($LASTEXITCODE -ne 0) { throw 'Invalid isolated local build configuration' }
    $plan = $planJson | ConvertFrom-Json
    foreach ($property in $plan.env.PSObject.Properties) {
        [Environment]::SetEnvironmentVariable($property.Name, [string]$property.Value, 'Process')
    }
    $buildArguments = @($plan.args)
    & node @buildArguments
    if ($LASTEXITCODE -ne 0) { throw 'Local-only Dendrite build failed' }
    $dest = Join-Path $PSScriptRoot 'dist/local-dev'
    New-Item -ItemType Directory -Path $dest -Force | Out-Null
    Copy-Item -LiteralPath 'target/debug/synthius-dendrite.exe' -Destination (Join-Path $dest 'synthius-dendrite.exe') -Force
    if (-not $NoBundle) {
        $version = (Get-Content tauri.conf.json -Raw | ConvertFrom-Json).version
        $overlay = $plan.env.TAURI_CONFIG | ConvertFrom-Json
        $installer = @(Get-ChildItem -LiteralPath 'target/debug/bundle/nsis' -Filter "$($overlay.productName)_${version}_x64-setup.exe")
        if ($installer.Count -ne 1) { throw 'Expected exactly one matching local-dev installer' }
        Copy-Item -LiteralPath $installer[0].FullName -Destination (Join-Path $dest 'SynthiusDendrite-local-dev-setup.exe') -Force
    }
    Write-Host 'Local-only Dendrite build: http://localhost:3000/files'
    Write-Host "Output: $dest"
} finally {
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name, $previous[$name], 'Process') }
    Pop-Location
}
