# Resume saved research, using the exact executable that created its checkpoints.
param([switch]$IncludeLargerRuns)
$ErrorActionPreference = 'Stop'
$researchRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $researchRoot
$env:PYTHONUTF8 = '1'
$env:PATH = (Join-Path $researchRoot '.venv\Scripts') + ';' + $env:PATH
$researchPython = Join-Path $researchRoot '.venv\Scripts\python.exe'
$frozenRunner = Join-Path $researchRoot '.local\strategy-ecosystem-20261014\frozen-deadline-runner\strategy_ecosystem_checkpoint_buffered.exe'
$pilotDirectory = Join-Path $researchRoot '.local\strategy-ecosystem-20261014\round2-deadline100'
$largeDirectory = Join-Path $researchRoot '.local\strategy-ecosystem-20261014\round2-deadline30s'
foreach ($directory in @($pilotDirectory, $largeDirectory)) {
    $resolvedDirectory = [IO.Path]::GetFullPath($directory)
    if (-not $resolvedDirectory.StartsWith($researchRoot + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Research directory escaped workspace'
    }
    $stopFile = Join-Path $resolvedDirectory 'STOP'
    if (Test-Path -LiteralPath $stopFile) { Remove-Item -LiteralPath $stopFile }
}
& $researchPython scripts/research_ecosystem_round2.py --cases adaptive-n5-s7 legacy-n5-s7 adaptive-n0-s7 adaptive-n10-s7 --count-override 100 --steps 480 --clock-seconds 60 --checkpoint --forks --skip-complete --binary $frozenRunner --output-dir $pilotDirectory
if ($LASTEXITCODE -ne 0) { throw 'Pilot research failed; receipts retained' }
& $researchPython scripts/analyze_ecosystem_round2.py --output-dir $pilotDirectory
if ($LASTEXITCODE -ne 0) { throw 'Pilot analysis failed' }
if ($IncludeLargerRuns) {
    & $researchPython scripts/research_ecosystem_round2.py --cases adaptive-n5-s7 legacy-n5-s7 adaptive-n0-s7 adaptive-n10-s7 adaptive-n5-s19 adaptive-1000-n5-s7 --steps 1440 --clock-seconds 30 --checkpoint --forks --skip-complete --binary $frozenRunner --output-dir $largeDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Larger research failed; checkpoints retained' }
    & $researchPython scripts/analyze_ecosystem_round2.py --output-dir $largeDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Larger analysis failed' }
}
