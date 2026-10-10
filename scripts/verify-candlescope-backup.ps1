param([string]$PostgresBin, [int]$PostgresPort = 55432, [int]$VerificationPort = 57307,
    [string]$RoomId = 'cs-persistent-ledger-20261008', [string]$BaselineDirectory, [string]$BackupPath)
$ErrorActionPreference = 'Stop'
$marketForgeRoot = Split-Path -Parent $PSScriptRoot
$runtimeRoot = Join-Path $marketForgeRoot '.local\candlescope-runtime'
if (!$BaselineDirectory) { $BaselineDirectory = Join-Path $marketForgeRoot 'output\candlescope-workbench' }
if (!$PostgresBin) { $PostgresBin = Split-Path -Parent (Get-Command pg_restore.exe -ErrorAction Stop).Source }
if (Get-NetTCPConnection -State Listen -LocalPort $VerificationPort -ErrorAction SilentlyContinue) { throw 'Verification port is already in use.' }
$password = [System.Net.NetworkCredential]::new('', ((Get-Content (Join-Path $runtimeRoot 'postgres-password.dpapi') -Raw).Trim() | ConvertTo-SecureString)).Password
$dump = if ($BackupPath) { Get-Item -LiteralPath $BackupPath } else { Get-ChildItem (Join-Path $runtimeRoot 'backups') -Filter *.dump | Sort-Object LastWriteTime -Descending | Select-Object -First 1 }
if (!$dump) { throw 'Create a backup before verifying.' }
$database = 'marketforge_restore_' + [guid]::NewGuid().ToString('N').Substring(0, 10)
$previousPassword = $env:PGPASSWORD
$previousDatabase = $env:MARKETFORGE_DATABASE_URL
$previousBind = $env:MARKETFORGE_BIND_ADDR
$server = $null
try {
$env:PGPASSWORD = $password
$connectionArgs = @('-h', '127.0.0.1', '-p', "$PostgresPort", '-U', 'marketforge', '-w')
& (Join-Path $PostgresBin 'createdb.exe') @connectionArgs $database
if ($LASTEXITCODE -ne 0) { throw 'Fresh verification database creation failed.' }
Write-Output "Created fresh verification database: $database"
& (Join-Path $PostgresBin 'pg_restore.exe') @connectionArgs -d $database $dump.FullName
if ($LASTEXITCODE -ne 0) { throw 'Archive restore failed.' }
Write-Output 'Backup restored into the new verification database.'
$env:MARKETFORGE_DATABASE_URL = "postgres://marketforge:$password@127.0.0.1:$PostgresPort/$database"
$env:MARKETFORGE_BIND_ADDR = "127.0.0.1:$VerificationPort"
$executable = Join-Path $marketForgeRoot 'target\candlescope-workbench\debug\exchange-server.exe'
$server = Start-Process -FilePath $executable -WorkingDirectory $marketForgeRoot -WindowStyle Hidden -PassThru `
    -RedirectStandardOutput (Join-Path $runtimeRoot 'restore-server.stdout.log') `
    -RedirectStandardError (Join-Path $runtimeRoot 'restore-server.stderr.log')
    $baseUrl = "http://127.0.0.1:$VerificationPort"
    $ready = $false
    for ($attempt = 0; $attempt -lt 30; $attempt++) {
        try { $null = Invoke-RestMethod "$baseUrl/health/ready" -TimeoutSec 1; $ready = $true; break }
        catch { Start-Sleep -Milliseconds 200 }
    }
    if (!$ready) { throw 'Restored server did not become ready.' }
    $matches = @()
    foreach ($endpoint in @('observe?account_id=20', 'clock', 'orders', 'candles?interval_ms=1000')) {
        $name = ($endpoint -split '\?')[0]
        $response = Invoke-WebRequest ("$baseUrl/rooms/" + [Uri]::EscapeDataString($RoomId) + '/' + $endpoint) -UseBasicParsing
        $before = Get-Content (Join-Path $BaselineDirectory ("persistent-before-" + $name + '.json')) -Raw
        if ($before -ne $response.Content) { throw "Restored $name differs from the saved acceptance fixture." }
        $matches += $name
    }
    @{ database=$database; backup=$dump.FullName; matches=$matches } | ConvertTo-Json | Set-Content (Join-Path $marketForgeRoot 'output\candlescope-workbench\backup-restore-proof.json')
    Write-Output ("Restored API matches: " + ($matches -join ', '))
} finally {
    $env:PGPASSWORD = $previousPassword
    $env:MARKETFORGE_DATABASE_URL = $previousDatabase
    $env:MARKETFORGE_BIND_ADDR = $previousBind
    if ($server) {
    $owned = Get-Process -Id $server.Id -ErrorAction SilentlyContinue
    if ($owned -and $owned.Path -eq $executable) { Stop-Process -Id $server.Id }
    }
}
