function Get-WorkbenchDatabaseUrl {
    param([string]$RuntimeRoot, [string]$PostgresBin, [int]$Port = 55432, [ValidatePattern("^[a-z][a-z0-9_]{0,62}$")][string]$DatabaseName = "marketforge_workbench")
    $ErrorActionPreference = 'Stop'
    if (!$PostgresBin) {
        $PostgresBin = Split-Path -Parent (Get-Command pg_ctl.exe -ErrorAction Stop).Source
    }
    foreach ($binary in @('pg_ctl.exe', 'initdb.exe', 'psql.exe', 'createdb.exe')) {
        if (!(Test-Path -LiteralPath (Join-Path $PostgresBin $binary))) { throw "PostgreSQL executable missing: $binary" }
    }
    New-Item -ItemType Directory -Force -Path $RuntimeRoot | Out-Null
    $clusterPath = Join-Path $RuntimeRoot 'postgres'
    $credentialPath = Join-Path $RuntimeRoot 'postgres-password.dpapi'
    if (!(Test-Path -LiteralPath $credentialPath)) {
        if (Test-Path -LiteralPath (Join-Path $clusterPath 'PG_VERSION')) { throw 'Existing cluster has no saved credential; refusing to replace it.' }
        $password = ([guid]::NewGuid().ToString('N') + [guid]::NewGuid().ToString('N'))
        ConvertTo-SecureString -String $password -AsPlainText -Force | ConvertFrom-SecureString | Set-Content -LiteralPath $credentialPath -Encoding ASCII
    }
    $password = [System.Net.NetworkCredential]::new('', ((Get-Content -LiteralPath $credentialPath -Raw).Trim() | ConvertTo-SecureString)).Password
    if (!(Test-Path -LiteralPath (Join-Path $clusterPath 'PG_VERSION'))) {
        $temporaryPassword = Join-Path $RuntimeRoot 'init-password.tmp'
        try {
            [System.IO.File]::WriteAllText($temporaryPassword, $password)
            & (Join-Path $PostgresBin 'initdb.exe') -D $clusterPath -U marketforge --encoding=UTF8 --auth-host=scram-sha-256 --auth-local=scram-sha-256 --pwfile=$temporaryPassword *> (Join-Path $RuntimeRoot 'initdb.log')
            if ($LASTEXITCODE -ne 0) { throw 'PostgreSQL initialization failed. Check initdb.log.' }
        } finally { if (Test-Path -LiteralPath $temporaryPassword) { Remove-Item -LiteralPath $temporaryPassword } }
    }
    & (Join-Path $PostgresBin 'pg_ctl.exe') status -D $clusterPath *> $null
    if ($LASTEXITCODE -ne 0) {
        if (Get-NetTCPConnection -State Listen -LocalPort $Port -ErrorAction SilentlyContinue) { throw "Port $Port is occupied by another database. Select another PostgresPort." }
        # pg_ctl may start a detached console on Windows; always launch its helper hidden.
        $arguments = @('start', '-D', ('"' + $clusterPath + '"'), '-l', ('"' + (Join-Path $RuntimeRoot 'postgres.log') + '"'), '-o', ('"-h 127.0.0.1 -p ' + $Port + '"'), '-w', '-t', '30')
        $startup = Start-Process -FilePath (Join-Path $PostgresBin 'pg_ctl.exe') -ArgumentList $arguments -WindowStyle Hidden -PassThru
        if (!$startup.WaitForExit(30000)) { throw 'PostgreSQL startup helper timed out. Check postgres.log.' }
        if ($startup.ExitCode -ne 0) { throw 'Project PostgreSQL failed to start. Check postgres.log.' }
    }
    $priorPassword = $env:PGPASSWORD
    try {
        $env:PGPASSWORD = $password
        $connectionArgs = @('-h', '127.0.0.1', '-p', "$Port", '-U', 'marketforge', '-w')
        $actualCluster = & (Join-Path $PostgresBin 'psql.exe') @connectionArgs -d postgres -Atc 'SHOW data_directory'
        if ($LASTEXITCODE -ne 0 -or [System.IO.Path]::GetFullPath($actualCluster.Trim()) -ne [System.IO.Path]::GetFullPath($clusterPath)) { throw 'PostgreSQL identity check failed; refusing to use another cluster.' }
        $exists = & (Join-Path $PostgresBin 'psql.exe') @connectionArgs -d postgres -Atc "SELECT 1 FROM pg_database WHERE datname='$DatabaseName'"
        if ($LASTEXITCODE -ne 0) { throw 'Database check failed' }
        if ($exists -ne '1') {
            & (Join-Path $PostgresBin 'createdb.exe') @connectionArgs $DatabaseName
            if ($LASTEXITCODE -ne 0) { throw 'Workbench database creation failed' }
        }
    } finally { $env:PGPASSWORD = $priorPassword }
    Write-Host "Project PostgreSQL ready: 127.0.0.1:$Port ($clusterPath)"
    return "postgres://marketforge:$password@127.0.0.1:$Port/$DatabaseName"
}
