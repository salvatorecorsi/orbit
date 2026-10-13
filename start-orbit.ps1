$projectDirectory = $PSScriptRoot
$executablePath = Join-Path $projectDirectory 'target\debug\orbit-rust.exe'
$runtimeDirectory = Join-Path $projectDirectory '.runtime'
if (-not (Test-Path -LiteralPath $executablePath)) {
    throw 'Esegui cargo build nella cartella del progetto prima di avviare ORBIT.'
}
New-Item -ItemType Directory -Path $runtimeDirectory -Force | Out-Null
Start-Process -FilePath $executablePath -WorkingDirectory $projectDirectory -WindowStyle Normal -RedirectStandardOutput (Join-Path $runtimeDirectory 'orbit.log') -RedirectStandardError (Join-Path $runtimeDirectory 'orbit-errors.log')
