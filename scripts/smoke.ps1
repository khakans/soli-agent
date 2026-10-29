$ErrorActionPreference='Stop'
$repo=Split-Path -Parent $PSScriptRoot
$binary=Join-Path $repo 'target/debug/soli-agent.exe'
$testDir=Join-Path $repo ('data/smoke-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
$names=@('SOLI_API_TOKEN','SOLI_DRY_RUN','SOLI_DB','SOLI_RPC_URL','SOLI_WS_URL','JUPITER_API_KEY','SOLI_BRIDGE_SECRETS_JSON','SOLI_OWNERS_JSON','SOLI_AUTO_PAPER','SOLI_SMART_WALLETS','SOLI_WATCH_MINTS','SOLI_STRATEGY_FILE')
$saved=@{}; foreach($name in $names){$saved[$name]=[Environment]::GetEnvironmentVariable($name);[Environment]::SetEnvironmentVariable($name,$null)}
$process=$null
function Start-SoliTest {
    $p=Start-Process -FilePath $binary -ArgumentList 'serve' -WorkingDirectory $repo -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $testDir 'stdout.log') -RedirectStandardError (Join-Path $testDir 'stderr.log')
    for($i=0;$i -lt 80;$i++){
        if($p.HasExited){throw 'Soli exited before readiness'}
        try{$health=Invoke-RestMethod 'http://127.0.0.1:8787/health';if($health.mode -eq 'dry_run'){return $p}}catch{}
        Start-Sleep -Milliseconds 100
    }
    Stop-Process -Id $p.Id -Force; throw 'Soli readiness timeout'
}
try {
    $env:SOLI_API_TOKEN=[Convert]::ToHexString([Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
    $env:SOLI_DRY_RUN='true';$env:SOLI_DB=Join-Path $testDir 'ledger.db'
    $headers=@{Authorization='Bearer '+$env:SOLI_API_TOKEN}
    $process=Start-SoliTest
    $unauthorized=$false
    try{$null=Invoke-RestMethod 'http://127.0.0.1:8787/v1/status'}catch{$unauthorized=[int]$_.Exception.Response.StatusCode -eq 401}
    if(!$unauthorized){throw 'Missing token was not rejected'}
    $status=Invoke-RestMethod 'http://127.0.0.1:8787/v1/status' -Headers $headers
    if($status.mode -ne 'dry_run' -or $status.live_enabled -or $status.ledger.cash_usdc -ne 1000000000){throw 'Unexpected initial paper status'}
    $null=Invoke-RestMethod 'http://127.0.0.1:8787/v1/halt' -Method Post -Headers $headers -ContentType 'application/json' -Body '{"halted":true}'
    Stop-Process -Id $process.Id -Force; $process.WaitForExit();$process=Start-SoliTest
    $status=Invoke-RestMethod 'http://127.0.0.1:8787/v1/status' -Headers $headers
    if(!$status.ledger.halted){throw 'Halt did not survive restart'}
    $report=Invoke-RestMethod 'http://127.0.0.1:8787/v1/report' -Headers $headers
    if($report.mode -ne 'dry_run'){throw 'Report mode mismatch'}
    & $binary status | Out-Null
    if($LASTEXITCODE -ne 0){throw 'Rust CLI bridge failed'}
    Write-Output 'PASS: local HTTP auth, dry-run mode, ledger persistence, report and Rust CLI. No external transaction.'
} finally {
    if($process -and !$process.HasExited){Stop-Process -Id $process.Id -Force}
    foreach($name in $names){[Environment]::SetEnvironmentVariable($name,$saved[$name])}
}
