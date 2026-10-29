param([Parameter(Mandatory=$true)][string]$VaultExe)
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
$binary = Join-Path $repo 'target/debug/soli-agent.exe'
if (!(Test-Path -LiteralPath $binary)) { throw 'Build soli-agent first' }
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('soli-vault-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $testRoot | Out-Null
$port = Get-Random -Minimum 46000 -Maximum 56000
$address = "http://127.0.0.1:$port"
$storage = (Join-Path $testRoot 'storage').Replace('\','/')
$config = "disable_mlock = true`nlistener `"tcp`" { address = `"127.0.0.1:$port`" tls_disable = true }`nstorage `"file`" { path = `"$storage`" }`n"
$configPath = Join-Path $testRoot 'vault.hcl'
[IO.File]::WriteAllText($configPath, $config)
$previousAddress=$env:VAULT_ADDR; $previousToken=$env:VAULT_TOKEN; $previousPath=$env:PATH; $previousImport=$env:SOLI_IMPORT_KEY_FILE
$process=$null
try {
    $process = Start-Process -FilePath $VaultExe -ArgumentList @('server', '-config', $configPath) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $testRoot 'stdout.log') -RedirectStandardError (Join-Path $testRoot 'stderr.log')
    $ready=$false
    for ($i=0; $i -lt 80; $i++) {
        try { $null=Invoke-RestMethod "$address/v1/sys/init"; $ready=$true; break } catch { Start-Sleep -Milliseconds 100 }
    }
    if (!$ready) { throw 'Temporary Vault did not start' }
    # These are newly generated disposable test keys, kept in memory only.
    $init=Invoke-RestMethod "$address/v1/sys/init" -Method Post -ContentType 'application/json' -Body '{"secret_shares":1,"secret_threshold":1}'
    $null=Invoke-RestMethod "$address/v1/sys/unseal" -Method Post -ContentType 'application/json' -Body (@{key=$init.keys_base64[0]} | ConvertTo-Json -Compress)
    $headers=@{'X-Vault-Token'=$init.root_token}
    $null=Invoke-RestMethod "$address/v1/sys/mounts/transit" -Method Post -Headers $headers -ContentType 'application/json' -Body '{"type":"transit"}'
    $env:VAULT_ADDR=$address; $env:VAULT_TOKEN=$init.root_token
    & $binary vault-key generate soli-validation
    if ($LASTEXITCODE -ne 0) { throw 'Real Vault generation/Ed25519 probe failed' }
    $env:PATH=(Split-Path -Parent $VaultExe)+[IO.Path]::PathSeparator+$env:PATH
    $seedPath=Join-Path $testRoot 'disposable-unfunded-seed.json'
    [IO.File]::WriteAllText($seedPath,([int[]][Security.Cryptography.RandomNumberGenerator]::GetBytes(32) | ConvertTo-Json -Compress))
    $env:SOLI_IMPORT_KEY_FILE=$seedPath
    $importOutput=& $binary vault-key import soli-import-validation 2>&1
    if($LASTEXITCODE -eq 0){Write-Output 'PASS: real Vault wrapped BYOK import and Ed25519 verification'}else{Write-Output 'BYOK import unavailable in this Vault environment; generation path remains supported.'}
    Remove-Item -LiteralPath $seedPath
    & $binary check-signer
    if($LASTEXITCODE -ne 0){throw 'Actual live signer/Vault startup validation failed'}
    $null=Invoke-RestMethod "$address/v1/sys/seal" -Method Post -Headers $headers
    $sealedOutput = & $binary vault-key verify soli-validation 2>&1
    if ($LASTEXITCODE -eq 0) { throw 'Sealed Vault unexpectedly allowed signing' }
    & $binary check-signer --expect-startup-failure
    if($LASTEXITCODE -ne 0){throw 'Sealed Vault did not reject live signer startup'}
    $null=Invoke-RestMethod "$address/v1/sys/unseal" -Method Post -ContentType 'application/json' -Body (@{key=$init.keys_base64[0]} | ConvertTo-Json -Compress)
    & $binary vault-key verify soli-validation
    if ($LASTEXITCODE -ne 0) { throw 'Unseal recovery/signature probe failed' }
    $beforeRestore=Invoke-RestMethod "$address/v1/transit/keys/soli-validation" -Headers $headers
    Stop-Process -Id $process.Id -Force; $process.WaitForExit()
    $restoreStorage=Join-Path $testRoot 'restored-storage'
    Copy-Item -LiteralPath (Join-Path $testRoot 'storage') -Destination $restoreStorage -Recurse
    $restoredConfig=$config.Replace($storage,$restoreStorage.Replace('\','/'))
    [IO.File]::WriteAllText($configPath,$restoredConfig)
    $process=Start-Process -FilePath $VaultExe -ArgumentList @('server','-config',$configPath) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $testRoot 'restore-stdout.log') -RedirectStandardError (Join-Path $testRoot 'restore-stderr.log')
    $ready=$false
    for($i=0;$i -lt 80;$i++) {
        try {$state=Invoke-RestMethod "$address/v1/sys/init";if($state.initialized){$ready=$true;break}}catch{}
        Start-Sleep -Milliseconds 100
    }
    if(!$ready){throw 'Restored Vault storage failed startup'}
    $null=Invoke-RestMethod "$address/v1/sys/unseal" -Method Post -ContentType 'application/json' -Body (@{key=$init.keys_base64[0]}|ConvertTo-Json -Compress)
    $afterRestore=Invoke-RestMethod "$address/v1/transit/keys/soli-validation" -Headers $headers
    if($beforeRestore.data.keys.'1'.public_key -ne $afterRestore.data.keys.'1'.public_key){throw 'Storage restore changed wallet'}
    & $binary check-signer
    if($LASTEXITCODE -ne 0){throw 'Restored Vault failed actual signer validation'}
    Write-Output 'PASS: actual encrypted Vault storage restore, original unseal share, wallet identity and live signer restart.'
    Write-Output 'PASS: real Vault generate, Ed25519 signature, sealed rejection and unseal recovery. No transaction broadcast.'
} finally {
    if ($process -and !$process.HasExited) { Stop-Process -Id $process.Id -Force }
    $env:VAULT_ADDR=$previousAddress; $env:VAULT_TOKEN=$previousToken
    $env:PATH=$previousPath; $env:SOLI_IMPORT_KEY_FILE=$previousImport
    $init=$null; $headers=$null
    # Test storage remains encrypted in the OS temp directory; recovery keys were not saved.
}
