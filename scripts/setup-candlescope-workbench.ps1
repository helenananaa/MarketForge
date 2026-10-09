param(
    [string]$PythonExecutable = 'python',
    [switch]$SkipFrontend
)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$sourceRoot = Join-Path $projectRoot 'vendor\candlescope'
$runtimeRoot = Join-Path $projectRoot '.local\candlescope-runtime'
$environmentRoot = Join-Path $runtimeRoot 'analysis-env'
$python = Join-Path $environmentRoot 'Scripts\python.exe'
if (!(Test-Path -LiteralPath (Join-Path $sourceRoot 'UPSTREAM.json'))) { throw 'Missing project-owned CandleScope source snapshot' }
if (!(Test-Path -LiteralPath $python)) {
    & $PythonExecutable -c 'import sys; assert sys.version_info[:2] == (3,12), "The pinned script runtime bundles require Python 3.12"'
    if ($LASTEXITCODE -ne 0) { throw 'Specify an installed Python 3.12 interpreter with -PythonExecutable.' }
    & $PythonExecutable -m venv $environmentRoot
    if ($LASTEXITCODE -ne 0) { throw 'Cannot create the project-owned analysis environment' }
}
Push-Location (Join-Path $sourceRoot 'backend')
try {
    & $python -m pip install -r requirements.txt ../packages/candlescope-backtest-sdk
    if ($LASTEXITCODE -ne 0) { throw 'Analysis dependency installation failed' }
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
        & $python -c 'from app.first_party_plugin_bootstrap import ensure_first_party_plugins_from_environment; from app.core.version import APP_VERSION; result = ensure_first_party_plugins_from_environment(host_name="CandleScope", host_version=APP_VERSION); assert result.status in ("installed", "ready"), result.to_wire(); print(result.to_wire())'
        if ($LASTEXITCODE -ne 0) { throw 'Pinned Pyne/Pine runtime installation failed; inspect the bootstrap error before starting the workbench.' }
    } finally {
        foreach ($name in $localEnvironment.Keys) { [Environment]::SetEnvironmentVariable($name, $priorEnvironment[$name], 'Process') }
    }
} finally { Pop-Location }
if (!$SkipFrontend) {
    Push-Location (Join-Path $sourceRoot 'frontend')
    try {
        & npm.cmd ci --ignore-scripts
        if ($LASTEXITCODE -ne 0) { throw 'Workbench frontend dependency installation failed' }
    } finally { Pop-Location }
}
Write-Output 'Workbench dependencies are installed inside MarketForge. Run scripts/start-candlescope-workbench.ps1.'
