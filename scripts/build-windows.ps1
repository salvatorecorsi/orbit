[CmdletBinding()]
param(
    [string]$OutputPath,
    [string]$NodeExecutable,
    [string]$NodeVersion = '24.22.0',
    [string]$CodeExecutable,
    [string]$MsvcRuntimeDirectory,
    [switch]$SkipBuild,
    [switch]$SkipInstall
)

$ErrorActionPreference = 'Stop'
$repository = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$buildDirectory = Join-Path $repository ('.runtime\package-build-' + [guid]::NewGuid().ToString('N'))
$downloads = Join-Path $repository '.runtime\package-downloads'
$stage = Join-Path $buildDirectory 'payload'
$application = Join-Path $stage 'app'
$runtime = Join-Path $application 'runtime'
$launcherManifest = Join-Path $repository 'packaging\launcher\Cargo.toml'
if (-not $OutputPath) { $OutputPath = Join-Path $repository '.runtime\dist\Orbit.exe' }
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)
foreach ($directory in @($buildDirectory, $downloads, $runtime, (Split-Path -Parent $OutputPath))) {
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
}

function Invoke-Checked {
    param([string]$Executable, [string[]]$Arguments)
    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Executable failed with exit code $LASTEXITCODE" }
}

function Get-Download {
    param([string]$Uri, [string]$Filename)
    $destination = Join-Path $downloads $Filename
    if (-not (Test-Path -LiteralPath $destination)) {
        $pending = $destination + '.pending'
        Invoke-WebRequest -Uri $Uri -OutFile $pending
        Move-Item -LiteralPath $pending -Destination $destination
    }
    return $destination
}

function Copy-Tree {
    param([string]$Source, [string]$Destination, [string[]]$Exclude = @())
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    foreach ($item in Get-ChildItem -LiteralPath $Source -Force) {
        if ($Exclude -contains $item.Name) { continue }
        Copy-Item -LiteralPath $item.FullName -Destination $Destination -Recurse -Force
    }
}

function Remove-BundleDirectory {
    param([string]$Directory)
    $resolved = [System.IO.Path]::GetFullPath($Directory)
    $allowedRoot = [System.IO.Path]::GetFullPath((Join-Path $application 'node_modules')) + [System.IO.Path]::DirectorySeparatorChar
    if (-not $resolved.StartsWith($allowedRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to remove a path outside the dependency staging directory: $resolved"
    }
    if (Test-Path -LiteralPath $resolved) { Remove-Item -LiteralPath $resolved -Recurse -Force }
}

Push-Location $repository
try {
    if (-not $SkipInstall) {
        Invoke-Checked 'npm.cmd' @('ci', '--omit=dev', '--no-audit', '--no-fund')
    }
    if (-not $SkipBuild) { Invoke-Checked 'cargo' @('build', '--release', '--locked') }
    $nativeExecutable = Join-Path $repository 'target\release\orbit-rust.exe'
    if (-not (Test-Path -LiteralPath $nativeExecutable)) { throw "Release build missing: $nativeExecutable" }
    if (-not (Test-Path -LiteralPath (Join-Path $repository 'node_modules\ws\package.json'))) {
        throw 'Run npm ci before using -SkipInstall.'
    }
    Copy-Item -LiteralPath $nativeExecutable -Destination $application
    Get-ChildItem -LiteralPath (Split-Path -Parent $nativeExecutable) -File -Filter '*.dll' |
        Copy-Item -Destination $application
    Copy-Tree (Join-Path $repository 'services') (Join-Path $application 'services') @('.runtime', 'workspace')
    Get-ChildItem -LiteralPath (Join-Path $application 'services') -File -Filter '*.test.js' |
        Remove-Item -Force
    Copy-Tree (Join-Path $repository 'node_modules') (Join-Path $application 'node_modules')
    # These packages ship several platforms in the same npm tarball. Keep the full
    # Windows x64 runtime, including all its DLLs and workers; remove other CPUs/OSes.
    $onnxBinaries = Join-Path $application 'node_modules\onnxruntime-node\bin'
    foreach ($api in Get-ChildItem -LiteralPath $onnxBinaries -Directory) {
        foreach ($platform in Get-ChildItem -LiteralPath $api.FullName -Directory) {
            if ($platform.Name -ne 'win32') { Remove-BundleDirectory $platform.FullName; continue }
            foreach ($architecture in Get-ChildItem -LiteralPath $platform.FullName -Directory) {
                if ($architecture.Name -ne 'x64') { Remove-BundleDirectory $architecture.FullName }
            }
        }
    }
    $ptyBinaries = Join-Path $application 'node_modules\node-pty\prebuilds'
    foreach ($platform in Get-ChildItem -LiteralPath $ptyBinaries -Directory) {
        if ($platform.Name -ne 'win32-x64') { Remove-BundleDirectory $platform.FullName }
    }
    Get-ChildItem -LiteralPath (Join-Path $application 'node_modules') -Recurse -File -Filter '*.pdb' |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force }

    if ($NodeExecutable) {
        $NodeExecutable = [System.IO.Path]::GetFullPath($NodeExecutable)
        $NodeVersion = (& $NodeExecutable --version).TrimStart('v').Trim()
    } else {
        $nodeArchive = Get-Download "https://nodejs.org/dist/v$NodeVersion/node-v$NodeVersion-win-x64.zip" "node-v$NodeVersion-win-x64.zip"
        $nodeDirectory = Join-Path $downloads "node-v$NodeVersion-win-x64"
        if (-not (Test-Path -LiteralPath (Join-Path $nodeDirectory 'node.exe'))) {
            Expand-Archive -LiteralPath $nodeArchive -DestinationPath $downloads -Force
        }
        $NodeExecutable = Join-Path $nodeDirectory 'node.exe'
    }
    Copy-Item -LiteralPath $NodeExecutable -Destination (Join-Path $runtime 'node.exe')
    if ((& $NodeExecutable -p "process.platform + '/' + process.arch").Trim() -ne 'win32/x64') {
        throw 'The bundle requires a Windows x64 Node.js executable.'
    }
    $licenses = Join-Path $application 'licenses'
    New-Item -ItemType Directory -Path $licenses -Force | Out-Null
    $nodeLicense = Join-Path (Split-Path -Parent $NodeExecutable) 'LICENSE'
    if (-not (Test-Path -LiteralPath $nodeLicense)) {
        $nodeLicense = Get-Download "https://raw.githubusercontent.com/nodejs/node/v$NodeVersion/LICENSE" "node-v$NodeVersion-LICENSE.txt"
    }
    Copy-Item -LiteralPath $nodeLicense -Destination (Join-Path $licenses 'Node-LICENSE.txt')

    if (-not $CodeExecutable) {
        $codeCandidates = @(
            (Join-Path $env:LOCALAPPDATA 'Programs\Microsoft VS Code\bin\code-tunnel.exe'),
            (Join-Path $env:ProgramFiles 'Microsoft VS Code\bin\code-tunnel.exe')
        )
        $CodeExecutable = $codeCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    }
    if (-not $CodeExecutable) {
        $codeArchive = Get-Download 'https://code.visualstudio.com/sha/download?build=stable&os=cli-win32-x64' 'vscode-cli-win32-x64.zip'
        $codeDirectory = Join-Path $downloads 'vscode-cli'
        Expand-Archive -LiteralPath $codeArchive -DestinationPath $codeDirectory -Force
        $CodeExecutable = (Get-ChildItem -LiteralPath $codeDirectory -Filter 'code*.exe' -Recurse -File | Select-Object -First 1).FullName
    }
    if (-not $CodeExecutable -or -not (Test-Path -LiteralPath $CodeExecutable)) {
        throw 'VS Code CLI missing. Supply -CodeExecutable with the path to code-tunnel.exe.'
    }
    Copy-Item -LiteralPath $CodeExecutable -Destination (Join-Path $runtime 'code-tunnel.exe')
    $codeBinaryDirectory = Split-Path -Parent $CodeExecutable
    $codeInstallDirectory = Split-Path -Parent $codeBinaryDirectory
    $codeLicenseCandidates = @(
        (Join-Path $codeBinaryDirectory 'LICENSE'),
        (Join-Path $codeInstallDirectory 'resources\app\LICENSE.rtf'),
        (Join-Path $codeInstallDirectory 'resources\app\LICENSE.txt')
    )
    foreach ($versionDirectory in Get-ChildItem -LiteralPath $codeInstallDirectory -Directory) {
        $codeLicenseCandidates += Join-Path $versionDirectory.FullName 'resources\app\LICENSE.rtf'
    }
    $codeLicense = $codeLicenseCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $codeLicense) {
        $codeLicense = Get-Download 'https://code.visualstudio.com/license' 'VS-Code-LICENSE.html'
    }
    Copy-Item -LiteralPath $codeLicense -Destination (Join-Path $licenses ('VS-Code-LICENSE' + [System.IO.Path]::GetExtension($codeLicense)))

    $webviewBootstrapper = Get-Download 'https://go.microsoft.com/fwlink/p/?LinkId=2124703' 'MicrosoftEdgeWebview2Setup.exe'
    $signature = Get-AuthenticodeSignature -LiteralPath $webviewBootstrapper
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Microsoft Corporation') {
        throw 'Microsoft WebView2 bootstrapper signature validation failed.'
    }
    Copy-Item -LiteralPath $webviewBootstrapper -Destination $runtime

    if (-not $MsvcRuntimeDirectory) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        if (Test-Path -LiteralPath $vswhere) {
            $visualStudio = & $vswhere -latest -products '*' -property installationPath
            if ($visualStudio) {
                $redist = Join-Path $visualStudio 'VC\Redist\MSVC'
                $MsvcRuntimeDirectory = Get-ChildItem -LiteralPath $redist -Directory |
                    Where-Object { $_.Name -match '^\d' } |
                    Sort-Object Name -Descending |
                    ForEach-Object { Get-ChildItem -LiteralPath (Join-Path $_.FullName 'x64') -Directory -Filter 'Microsoft.VC*.CRT' } |
                    Select-Object -First 1 -ExpandProperty FullName
            }
        }
    }
    if (-not $MsvcRuntimeDirectory) { throw 'MSVC redistributable DLL directory not found. Supply -MsvcRuntimeDirectory.' }
    Get-ChildItem -LiteralPath $MsvcRuntimeDirectory -File -Filter '*.dll' | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination $runtime
        Copy-Item -LiteralPath $_.FullName -Destination $application
    }
    Push-Location $application
    try {
        Invoke-Checked (Join-Path $runtime 'node.exe') @('-e', "require('node-pty');require('@napi-rs/canvas');require('onnxruntime-node');require('ws');console.log('Bundled native modules loaded successfully')")
    } finally { Pop-Location }

    $manifest = [ordered]@{
        format = 1
        architecture = 'windows-x64'
        builtAt = [DateTime]::UtcNow.ToString('o')
        sourceCommit = (& git rev-parse HEAD).Trim()
        nodeVersion = (& $NodeExecutable --version).Trim()
        codeVersion = (& $CodeExecutable --version | Out-String).Trim()
        note = 'WebView2 installs automatically if absent; VS Code server and AI model caches download on first use. AI CLI authentication uses the current Windows account.'
    }
    $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $application 'bundle.json') -Encoding utf8
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $payload = Join-Path $buildDirectory 'payload.zip'
    [System.IO.Compression.ZipFile]::CreateFromDirectory($stage, $payload, [System.IO.Compression.CompressionLevel]::Optimal, $false)
    $payloadHash = (Get-FileHash -LiteralPath $payload -Algorithm SHA256).Hash.ToLowerInvariant()
    $savedPayload = $env:ORBIT_PAYLOAD_ZIP
    $savedHash = $env:ORBIT_PAYLOAD_SHA256
    $savedRustFlags = $env:RUSTFLAGS
    try {
        $env:ORBIT_PAYLOAD_ZIP = $payload
        $env:ORBIT_PAYLOAD_SHA256 = $payloadHash
        $env:RUSTFLAGS = ($savedRustFlags + ' -C target-feature=+crt-static').Trim()
        Invoke-Checked 'cargo' @('build', '--release', '--manifest-path', $launcherManifest, '--target-dir', (Join-Path $repository 'packaging\launcher\target'), '--locked')
    } finally {
        $env:ORBIT_PAYLOAD_ZIP = $savedPayload
        $env:ORBIT_PAYLOAD_SHA256 = $savedHash
        $env:RUSTFLAGS = $savedRustFlags
    }
    $launcher = Join-Path $repository 'packaging\launcher\target\release\orbit-launcher.exe'
    Copy-Item -LiteralPath $launcher -Destination $OutputPath -Force
    $result = Get-Item -LiteralPath $OutputPath
    Write-Output ('Created ' + $result.FullName + ' (' + [Math]::Round($result.Length / 1MB, 1) + ' MiB)')
    Write-Output ('SHA256 ' + (Get-FileHash -LiteralPath $OutputPath -Algorithm SHA256).Hash.ToLowerInvariant())
    Write-Output ('Payload ' + $payloadHash)
} finally {
    Pop-Location
}
