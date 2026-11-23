# Installs Kairos for the current user: irm https://kairos.obayy.dev/install.ps1 | iex
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('kairos-' + [guid]::NewGuid().ToString('N'))
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $server = 'https://kairos.obayy.dev'
    if ($env:KAIROS_SERVER) { $server = $env:KAIROS_SERVER.TrimEnd('/') }
    if ($env:PROCESSOR_ARCHITECTURE -eq 'x86' -and -not $env:PROCESSOR_ARCHITEW6432) {
        throw 'Kairos needs a 64-bit version of Windows.'
    }
    # Windows on ARM runs the x64 build through emulation.
    $target = 'windows-x86_64'

    New-Item -ItemType Directory -Path $tmp | Out-Null
    Write-Host 'Looking up the latest Kairos...'
    $manifestFile = Join-Path $tmp 'manifest.json'
    try {
        Invoke-WebRequest -UseBasicParsing -Uri "$server/download/latest/manifest.json" -OutFile $manifestFile
    } catch {
        throw "Kairos could not reach $server. Check your internet connection and try again."
    }
    $manifest = Get-Content -Raw $manifestFile | ConvertFrom-Json
    $version = $manifest.version
    if ($version -notmatch '^\d+\.\d+\.\d+') { throw "the download list from $server looks broken. Try again in a few minutes." }
    $files = $manifest.targets.$target.files
    if (-not $files) { throw "Kairos $version has no download for $target." }

    Write-Host "Downloading Kairos $version for $target..."
    foreach ($name in 'kairos.exe', 'kairosd.exe') {
        $info = $files.$name
        if (-not $info) { throw "Kairos $version has no $name for $target." }
        $file = Join-Path $tmp $name
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$server/download/$version/$target/$name" -OutFile $file
        } catch {
            throw "$name could not be downloaded. Try again in a few minutes."
        }
        if ((Get-Item $file).Length -ne [int64]$info.size) {
            throw "the downloaded $name is incomplete, so nothing was installed. Try again."
        }
        if ((Get-FileHash -Algorithm SHA256 -Path $file).Hash -ne $info.sha256) {
            throw "the downloaded $name does not match its checksum, so nothing was installed."
        }
    }

    & (Join-Path $tmp 'kairos.exe') install --server $server
    if ($LASTEXITCODE -ne 0) { throw 'the install did not finish.' }
    $kairosDir = Join-Path $env:LOCALAPPDATA 'Programs\Kairos'
    if (($env:Path -split ';').TrimEnd('\') -notcontains $kairosDir) { $env:Path = $env:Path.TrimEnd(';') + ';' + $kairosDir }
} catch {
    Write-Host "Sorry, $($_.Exception.Message)" -ForegroundColor Red
} finally {
    if (Test-Path $tmp) { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
}
