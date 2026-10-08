param([string]$DatabaseUrl = $env:MARKETFORGE_DATABASE_URL, [string]$PostgresBin, [int]$PostgresPort = 55432, [string]$BackupPath)
$ErrorActionPreference = 'Stop'
$marketForgeRoot = Split-Path -Parent $PSScriptRoot
$runtimeRoot = Join-Path $marketForgeRoot '.local\candlescope-runtime'
if (!$PostgresBin) { $PostgresBin = Split-Path -Parent (Get-Command pg_dump.exe -ErrorAction Stop).Source }
if (!$DatabaseUrl) {
    $credentialPath = Join-Path $runtimeRoot 'postgres-password.dpapi'
    $password = [System.Net.NetworkCredential]::new('', ((Get-Content -LiteralPath $credentialPath -Raw).Trim() | ConvertTo-SecureString)).Password
    $DatabaseUrl = "postgres://marketforge:$password@127.0.0.1:$PostgresPort/marketforge_workbench"
}
$uri = [Uri]$DatabaseUrl
if ($uri.Scheme -notin @('postgres', 'postgresql') -or $uri.Query) { throw 'Use a PostgreSQL URL without query options for this backup helper.' }
$identity = $uri.UserInfo.Split(':', 2)
if (!$BackupPath) { $BackupPath = Join-Path $runtimeRoot ("backups\workbench-" + (Get-Date -Format 'yyyyMMdd-HHmmss-fff') + '.dump') }
if (Test-Path -LiteralPath $BackupPath) { throw 'Refusing to overwrite an existing backup.' }
New-Item -ItemType Directory -Force -Path (Split-Path -Parent ([System.IO.Path]::GetFullPath($BackupPath))) | Out-Null
$priorPassword = $env:PGPASSWORD
try {
    $env:PGPASSWORD = if ($identity.Length -eq 2) { [Uri]::UnescapeDataString($identity[1]) } else { $null }
    & (Join-Path $PostgresBin 'pg_dump.exe') -h $uri.Host -p $uri.Port -U ([Uri]::UnescapeDataString($identity[0])) -w -d $uri.AbsolutePath.Trim('/') -Fc -f $BackupPath
    if ($LASTEXITCODE -ne 0) { throw 'Database backup failed. Do not use the partial dump.' }
    & (Join-Path $PostgresBin 'pg_restore.exe') -l $BackupPath *> $null
    if ($LASTEXITCODE -ne 0) { throw 'Backup archive verification failed' }
} finally { $env:PGPASSWORD = $priorPassword }
Write-Output "Verified backup: $BackupPath"
