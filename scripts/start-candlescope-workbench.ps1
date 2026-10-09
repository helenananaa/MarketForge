param(
    [switch]$SkipBuild,
    [string]$DatabaseUrl = $env:MARKETFORGE_DATABASE_URL,
    [string]$PostgresBin,
    [string]$DatabaseName,
    [int]$PostgresPort = 55432,
    [int]$BackendPort = 0,
    [int]$IndicatorPort = 18086,
    [string]$PythonExecutable,
    [ValidateSet("accounts", "local-development")]
    [string]$AuthMode = "accounts",
    [switch]$Memory
)
$ErrorActionPreference = 'Stop'
if (!$BackendPort) { $BackendPort = if ($AuthMode -eq 'accounts') { 57307 } else { 57306 } }
if (!$DatabaseName) { $DatabaseName = if ($AuthMode -eq 'accounts') { 'marketforge_competition' } else { 'marketforge_workbench' } }
$marketForgeRoot = Split-Path -Parent $PSScriptRoot
$CandleScopeRoot = Join-Path $marketForgeRoot 'vendor\candlescope'
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
        $DatabaseUrl = Get-WorkbenchDatabaseUrl -RuntimeRoot $runtimeRoot -PostgresBin $PostgresBin -Port $PostgresPort -DatabaseName $DatabaseName
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
    $priorAuthMode = $env:MARKETFORGE_AUTH_MODE
    try {
    $env:MARKETFORGE_DATABASE_URL = if ($Memory) { $null } else { $DatabaseUrl }
    $env:MARKETFORGE_BIND_ADDR = "127.0.0.1:$BackendPort"
    $env:MARKETFORGE_AUTH_MODE = $AuthMode
    $backendProcess = Start-Process -FilePath (Join-Path $backendTarget 'debug\exchange-server.exe') `
        -WorkingDirectory $marketForgeRoot -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $outputDir 'launcher-backend.stdout.log') `
        -RedirectStandardError (Join-Path $outputDir 'launcher-backend.stderr.log')
    } finally { $env:MARKETFORGE_DATABASE_URL = $priorDatabase; $env:MARKETFORGE_BIND_ADDR = $priorBind; $env:MARKETFORGE_AUTH_MODE = $priorAuthMode }
    Write-Output "MarketForge process: $($backendProcess.Id)"
}
# Reuse a compatible running backend. Never stop another service occupying its port.
$backendReady = $false
for ($attempt = 0; $attempt -lt 30; $attempt++) {
    try {
        $runtime = Invoke-RestMethod "$backendUrl/runtime" -TimeoutSec 1
        $backendReady = $runtime.websocket.version -eq 'simulation.ws.v1' -and $runtime.room_portal.version -eq 'room.portal.v1' -and $runtime.competition_platform.version -eq 'competition.v1' -and $runtime.competition_platform.auth_mode -eq $AuthMode
        if ($backendReady) {
            $null = Invoke-RestMethod "$backendUrl/health/ready" -TimeoutSec 1
            break
        }
    } catch { $backendReady = $false; Start-Sleep -Milliseconds 200 }
}
if (!$backendReady) { throw 'MarketForge is unavailable, requires credentials, or predates the room portal API. Rebuild/restart the project-owned backend and check its configuration. No existing process was stopped.' }
if (!$Memory -and !$runtime.storage.durable) { throw 'Existing backend is in memory mode; choose another BackendPort or explicitly use -Memory. No process was stopped.' }
# Indicators, custom script catalog and sidecars use the project-owned CandleScope copy.
$indicatorListener = Get-NetTCPConnection -LocalPort $IndicatorPort -State Listen -ErrorAction SilentlyContinue
if ($indicatorListener) {
    $indicatorOwner = Get-CimInstance Win32_Process -Filter "ProcessId=$($indicatorListener[0].OwningProcess)"
    if (!$indicatorOwner.CommandLine.Contains((Join-Path $CandleScopeRoot 'backend'), [StringComparison]::OrdinalIgnoreCase)) {
        throw 'IndicatorPort belongs to another service. Choose a different IndicatorPort; the workbench cannot reuse an external checkout.'
    }
}
if (!$indicatorListener) {
    $candlePhysicalRoot = Split-Path -Parent $frontendRoot
    if (!$PythonExecutable) {
        foreach ($candidate in @((Join-Path $runtimeRoot 'analysis-env\Scripts\python.exe'))) {
            if (Test-Path -LiteralPath $candidate) { $PythonExecutable = $candidate; break }
        }
    }
    if (!$PythonExecutable) { throw 'Install the project-owned environment with scripts/setup-candlescope-workbench.ps1, or specify -PythonExecutable.' }
    $priorPythonPath = $env:PYTHONPATH
    $localEnvironment = @{
        CANDLE_DATA_DIR = (Join-Path $runtimeRoot 'analysis-data')
        CANDLESCOPE_RUNTIME_MODE = 'LOCAL_OFFLINE'
        CANDLESCOPE_RUNTIME_REGISTRY = (Join-Path $runtimeRoot 'analysis-plugins\runtime-registry.json')
        CANDLESCOPE_PLUGIN_DOWNLOAD_CACHE = (Join-Path $runtimeRoot 'analysis-plugin-downloads')
        CANDLESCOPE_PLUGIN_PLATFORM_V2_ROOT = (Join-Path $runtimeRoot 'analysis-platform')
    }
    $priorEnvironment = @{}
    try {
        foreach ($name in $localEnvironment.Keys) {
            $priorEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
            [Environment]::SetEnvironmentVariable($name, $localEnvironment[$name], 'Process')
        }
        $env:PYTHONPATH = @((Join-Path $candlePhysicalRoot 'packages\candlescope-plugin-sdk\src'), (Join-Path $candlePhysicalRoot 'packages\candlescope-backtest-sdk\src'), $priorPythonPath) -join [IO.Path]::PathSeparator
        $indicatorProcess = Start-Process -FilePath $PythonExecutable -ArgumentList @('-m', 'uvicorn', 'app.marketforge_analysis:app', '--app-dir', ('"' + (Join-Path $CandleScopeRoot 'backend') + '"'), '--host', '127.0.0.1', '--port', $IndicatorPort) `
            -WorkingDirectory (Join-Path $candlePhysicalRoot 'backend') -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput (Join-Path $outputDir 'launcher-indicators.stdout.log') `
            -RedirectStandardError (Join-Path $outputDir 'launcher-indicators.stderr.log')
        Write-Output "CandleScope indicator process: $($indicatorProcess.Id)"
    } finally {
        $env:PYTHONPATH = $priorPythonPath
        foreach ($name in $localEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $priorEnvironment[$name], 'Process') }
    }
}
$indicatorReady = $false
for ($attempt = 0; $attempt -lt 90; $attempt++) {
    try {
        $presets = Invoke-RestMethod "http://127.0.0.1:$IndicatorPort/api/v1/indicators/presets" -TimeoutSec 1
        $analysisHealth = Invoke-RestMethod "http://127.0.0.1:$IndicatorPort/health" -TimeoutSec 1
        $indicatorReady = $null -ne $presets -and $analysisHealth.kind -eq 'marketforge-analysis' -and $analysisHealth.source_root -eq (Join-Path $CandleScopeRoot 'backend')
        if ($indicatorReady) { break }
    } catch { Start-Sleep -Milliseconds 500 }
}
if (!$indicatorReady) { throw 'CandleScope indicator API is unavailable. Inspect launcher-indicators logs.' }
$frontendPort = Get-NetTCPConnection -LocalPort 15173 -State Listen -ErrorAction SilentlyContinue
if ($frontendPort) {
    $frontendOwner = Get-CimInstance Win32_Process -Filter "ProcessId=$($frontendPort[0].OwningProcess)"
    if (!$frontendOwner.CommandLine.Contains($frontendRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Port 15173 belongs to another frontend. The workbench cannot reuse an external checkout.'
    }
}
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
    $priorApiProxy = $env:VITE_API_PROXY_TARGET
    try {
    $env:VITE_API_PROXY_TARGET = "http://127.0.0.1:$IndicatorPort"
    $frontendProcess = Start-Process -FilePath (Get-Command node).Source `
        -ArgumentList @(('"' + (Join-Path $frontendRoot 'node_modules\vite\bin\vite.js') + '"'), 'preview', '--host', '127.0.0.1', '--port', '15173', '--strictPort') `
        -WorkingDirectory $frontendRoot -WindowStyle Hidden -PassThru `
        -RedirectStandardOutput (Join-Path $outputDir 'launcher-frontend.stdout.log') `
        -RedirectStandardError (Join-Path $outputDir 'launcher-frontend.stderr.log')
    } finally { $env:VITE_API_PROXY_TARGET = $priorApiProxy }
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
Write-Output ("Open http://127.0.0.1:15173/simulation.html?server=" + [Uri]::EscapeDataString($backendUrl))
Write-Output "Source: $CandleScopeRoot (MarketForge-owned copy)."
Write-Output "Backend: $backendUrl; storage: $($runtime.storage.kind)."
Write-Output "Indicators: CandleScope on port $IndicatorPort. Custom languages require their installed CandleScope runtime plugins."
