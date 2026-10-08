param(
    [string]$CandleScopeRoot = 'H:\program\CandleScope',
    [switch]$SkipBuild,
    [string]$DatabaseUrl = $env:MARKETFORGE_DATABASE_URL,
    [string]$PostgresBin,
    [int]$PostgresPort = 55432,
    [int]$BackendPort = 57306,
    [switch]$Memory
)
$ErrorActionPreference = 'Stop'
$marketForgeRoot = Split-Path -Parent $PSScriptRoot
$frontendRoot = Join-Path $CandleScopeRoot 'frontend'
if (!(Test-Path -LiteralPath (Join-Path $frontendRoot 'simulation.html'))) {
    throw "CandleScope simulation frontend is missing: $frontendRoot"
}
$resolveScript = "console.log(require('node:fs').realpathSync.native(process.argv[2]))"
$resolvedFrontend = $resolveScript | & node - $frontendRoot
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve the CandleScope frontend directory' }
$frontendRoot = $resolvedFrontend.Trim()
$outputDir = Join-Path $marketForgeRoot 'output\candlescope-workbench'
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
$runtimeRoot = Join-Path $marketForgeRoot '.local\candlescope-runtime'
$backendUrl = "http://127.0.0.1:$BackendPort"
$backendTarget = Join-Path $marketForgeRoot 'target\candlescope-workbench'
$backendListener = Get-NetTCPConnection -LocalPort $BackendPort -State Listen -ErrorAction SilentlyContinue
if (!$backendListener) {
    if (!$Memory -and !$DatabaseUrl) {
        . (Join-Path $PSScriptRoot 'candlescope-postgres.ps1')
        $DatabaseUrl = Get-WorkbenchDatabaseUrl -RuntimeRoot $runtimeRoot -PostgresBin $PostgresBin -Port $PostgresPort
    }
    if (!$SkipBuild) {
        Push-Location $marketForgeRoot
        try {
            & cargo build -p exchange-server --bin exchange-server --target-dir $backendTarget
            if ($LASTEXITCODE -ne 0) { throw 'MarketForge build failed' }
        } finally { Pop-Location }
    }
    $priorDatabase = $env:MARKETFORGE_DATABASE_URL
    $priorBind = $env:MARKETFORGE_BIND_ADDR
    try {
    $env:MARKETFORGE_DATABASE_URL = if ($Memory) { $null } else { $DatabaseUrl }
    $env:MARKETFORGE_BIND_ADDR = "127.0.0.1:$BackendPort"
    $backendProcess = Start-Process -FilePath (Join-Path $backendTarget 'debug\exchange-server.exe') `
        -WorkingDirectory $marketForgeRoot -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $outputDir 'launcher-backend.stdout.log') `
        -RedirectStandardError (Join-Path $outputDir 'launcher-backend.stderr.log')
    } finally { $env:MARKETFORGE_DATABASE_URL = $priorDatabase; $env:MARKETFORGE_BIND_ADDR = $priorBind }
    Write-Output "MarketForge process: $($backendProcess.Id)"
}
# Reuse a compatible running backend. Never stop another service occupying its port.
$backendReady = $false
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
        $runtime = Invoke-RestMethod "$backendUrl/runtime" -TimeoutSec 1
        $backendReady = $runtime.websocket.version -eq 'simulation.ws.v1'
        if ($backendReady) {
            $null = Invoke-RestMethod "$backendUrl/health/ready" -TimeoutSec 1
            break
        }
    } catch { $backendReady = $false; Start-Sleep -Milliseconds 200 }
}
if (!$backendReady) { throw 'MarketForge is unavailable, requires credentials, or predates the runtime/WebSocket API. Check the backend logs and configuration.' }
if (!$Memory -and !$runtime.storage.durable) { throw 'Existing backend is in memory mode; choose another BackendPort or explicitly use -Memory. No process was stopped.' }
$frontendPort = Get-NetTCPConnection -LocalPort 15173 -State Listen -ErrorAction SilentlyContinue
if (!$frontendPort) {
    Push-Location $frontendRoot
    try {
        if (!(Test-Path -LiteralPath 'node_modules\vite\bin\vite.js')) {
            & npm.cmd ci --ignore-scripts
            if ($LASTEXITCODE -ne 0) { throw 'CandleScope dependency installation failed' }
        }
        if (!$SkipBuild) {
            & npm.cmd run build
            if ($LASTEXITCODE -ne 0) { throw 'CandleScope build failed' }
        }
    } finally { Pop-Location }
    $frontendProcess = Start-Process -FilePath (Get-Command node).Source `
        -ArgumentList @(('"' + (Join-Path $frontendRoot 'node_modules\vite\bin\vite.js') + '"'), 'preview', '--host', '127.0.0.1', '--port', '15173', '--strictPort') `
        -WorkingDirectory $frontendRoot -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $outputDir 'launcher-frontend.stdout.log') `
        -RedirectStandardError (Join-Path $outputDir 'launcher-frontend.stderr.log')
    Write-Output "CandleScope frontend process: $($frontendProcess.Id)"
}
$frontendReady = $false
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
        $page = Invoke-WebRequest 'http://127.0.0.1:15173/simulation.html' -UseBasicParsing -TimeoutSec 1
        $frontendReady = $page.Content -match 'CandleScope.*MarketForge'
        if ($frontendReady) { break }
    } catch { Start-Sleep -Milliseconds 200 }
}
if (!$frontendReady) { throw 'CandleScope simulation page is unavailable on port 15173. Check the frontend build, port owner, and launcher logs.' }
Write-Output 'Open http://127.0.0.1:15173/simulation.html'
Write-Output "Backend: $backendUrl; storage: $($runtime.storage.kind). Set the service URL in the page if using a non-default BackendPort."
