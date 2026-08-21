#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [string]$ArtifactDirectory,

    [string]$Notes,

    [string]$SshHost = '142.132.199.184',

    [ValidateRange(1, 65535)]
    [int]$SshPort = 2222,

    [string]$SshUser = 'ayla',

    [string]$IdentityFile = "$env:USERPROFILE\.ssh\ayla-vm-ed25519",

    [string]$PublicBaseUrl = 'https://yl.xyne.gg/updates',

    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$platform = 'windows-x86_64'
$projectRoot = Split-Path -Parent $PSScriptRoot
$latestUrl = "$($PublicBaseUrl.TrimEnd('/'))/stable/latest.json"

function ConvertTo-StableSemVer {
    param([string]$Value, [string]$Source)

    $match = [regex]::Match(
        $Value.Trim(),
        '^(?:v)?(?<normalized>(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))$',
        [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
    )
    if (-not $match.Success) {
        throw "$Source must be a stable SemVer value such as '0.3.2'."
    }
    return $match.Groups['normalized'].Value
}

function Compare-StableSemVer {
    param([string]$Left, [string]$Right)

    $leftParts = @($Left.Split('.') | ForEach-Object { [uint64]::Parse($_) })
    $rightParts = @($Right.Split('.') | ForEach-Object { [uint64]::Parse($_) })
    for ($index = 0; $index -lt 3; $index++) {
        if ($leftParts[$index] -lt $rightParts[$index]) { return -1 }
        if ($leftParts[$index] -gt $rightParts[$index]) { return 1 }
    }
    return 0
}

function Get-CargoPackageVersion {
    param([string]$Path)

    $insidePackage = $false
    $versions = @()
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ($line -match '^\s*\[(?<section>[^]]+)\]\s*$') {
            if ($insidePackage) { break }
            $insidePackage = $Matches['section'] -eq 'package'
            continue
        }
        if ($insidePackage -and $line -match '^\s*version\s*=\s*"(?<version>[^"]+)"\s*(?:#.*)?$') {
            $versions += $Matches['version']
        }
    }
    if ($versions.Count -ne 1) {
        throw "Unable to identify exactly one [package] version in '$Path'."
    }
    return [string]$versions[0]
}

function Invoke-CheckedCommand {
    param([string]$Executable, [string[]]$Arguments, [string]$Operation)

    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Operation failed with exit code $LASTEXITCODE."
    }
}

function Get-CurrentPublishedVersion {
    try {
        $response = Invoke-WebRequest -Uri "$latestUrl`?check=$([guid]::NewGuid().ToString('N'))" `
            -Headers @{ 'Cache-Control' = 'no-cache'; 'User-Agent' = 'AylaSelfHostedPublisher/1.0' } `
            -MaximumRedirection 4 -TimeoutSec 30 -UseBasicParsing
    }
    catch {
        $statusCode = $null
        if ($_.Exception.PSObject.Properties.Name -contains 'Response' -and $_.Exception.Response) {
            $statusCode = [int]$_.Exception.Response.StatusCode
        }
        if ($statusCode -in @(204, 404)) { return $null }
        throw "Unable to read the current self-hosted update channel."
    }

    if ([int]$response.StatusCode -eq 204 -or [string]::IsNullOrWhiteSpace([string]$response.Content)) {
        return $null
    }
    try {
        $metadata = ConvertFrom-Json -InputObject ([string]$response.Content)
        return ConvertTo-StableSemVer -Value ([string]$metadata.version) -Source 'Published update version'
    }
    catch {
        throw 'The current self-hosted latest.json is invalid.'
    }
}

$normalizedVersion = ConvertTo-StableSemVer -Value $Version -Source 'Requested version'
$expectedInstallerName = "Ayla_${normalizedVersion}_x64-setup.exe"
$expectedSignatureName = "$expectedInstallerName.sig"
$artifactPath = (Get-Item -LiteralPath $ArtifactDirectory -Force).FullName

if (-not (Test-Path -LiteralPath $artifactPath -PathType Container)) {
    throw 'ArtifactDirectory must point to a directory.'
}
if (-not (Test-Path -LiteralPath $IdentityFile -PathType Leaf)) {
    throw "SSH identity file was not found: '$IdentityFile'."
}

$packageJson = Get-Content -LiteralPath (Join-Path $projectRoot 'package.json') -Raw | ConvertFrom-Json
$tauriConfig = Get-Content -LiteralPath (Join-Path $projectRoot 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json
$manifestVersions = [ordered]@{
    'package.json' = ConvertTo-StableSemVer -Value ([string]$packageJson.version) -Source 'package.json version'
    'src-tauri/tauri.conf.json' = ConvertTo-StableSemVer -Value ([string]$tauriConfig.version) -Source 'tauri.conf.json version'
    'src-tauri/Cargo.toml' = ConvertTo-StableSemVer `
        -Value (Get-CargoPackageVersion -Path (Join-Path $projectRoot 'src-tauri\Cargo.toml')) `
        -Source 'Cargo.toml package version'
}
foreach ($entry in $manifestVersions.GetEnumerator()) {
    if ($entry.Value -ne $normalizedVersion) {
        throw "Requested version '$normalizedVersion' does not match $($entry.Key) version '$($entry.Value)'."
    }
}

$installerPath = Join-Path $artifactPath $expectedInstallerName
$signaturePath = Join-Path $artifactPath $expectedSignatureName
foreach ($path in @($installerPath, $signaturePath)) {
    $item = Get-Item -LiteralPath $path -Force
    if ($item.PSIsContainer -or $item.Length -le 0 -or
        ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Release artifact '$path' must be a non-empty regular file."
    }
}

$productVersion = ConvertTo-StableSemVer `
    -Value ([string](Get-Item -LiteralPath $installerPath).VersionInfo.ProductVersion) `
    -Source 'NSIS ProductVersion'
if ($productVersion -ne $normalizedVersion) {
    throw "NSIS ProductVersion '$productVersion' does not match '$normalizedVersion'."
}

$cargo = Get-Command cargo -CommandType Application -ErrorAction Stop
$verifierManifest = Join-Path $projectRoot 'tools\update-verifier\Cargo.toml'
$verificationOutput = & $cargo.Source run --quiet --locked `
    --manifest-path $verifierManifest -- $installerPath $signaturePath ([string]$tauriConfig.plugins.updater.pubkey)
if ($LASTEXITCODE -ne 0 -or ([string]$verificationOutput).Trim() -ne 'signature verified') {
    throw 'The updater signature failed cryptographic verification.'
}

$currentVersion = Get-CurrentPublishedVersion
if ($null -ne $currentVersion -and
    (Compare-StableSemVer -Left $normalizedVersion -Right $currentVersion) -le 0) {
    throw "Version '$normalizedVersion' must be greater than published version '$currentVersion'."
}

if ([string]::IsNullOrWhiteSpace($Notes)) {
    $Notes = "Ayla $normalizedVersion Windows update."
}
$signature = [System.IO.File]::ReadAllText($signaturePath)
$immutableInstallerUrl = "$($PublicBaseUrl.TrimEnd('/'))/releases/$normalizedVersion/$expectedInstallerName"
$metadata = [ordered]@{
    version = $normalizedVersion
    notes = $Notes
    pub_date = [DateTimeOffset]::UtcNow.ToString("yyyy-MM-dd'T'HH:mm:ss'Z'")
    platforms = [ordered]@{
        $platform = [ordered]@{
            signature = $signature
            url = $immutableInstallerUrl
        }
    }
}
$metadataPath = Join-Path $artifactPath 'latest.json'
$utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
[System.IO.File]::WriteAllText(
    $metadataPath,
    (ConvertTo-Json -InputObject $metadata -Depth 6) + [Environment]::NewLine,
    $utf8WithoutBom
)

$installerHash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
$installerSize = (Get-Item -LiteralPath $installerPath).Length
Write-Output "Prepared Ayla $normalizedVersion for $immutableInstallerUrl"
Write-Output "Installer SHA-256: $installerHash"
if ($DryRun) {
    Write-Output 'Dry run complete. The server was not modified.'
    return
}

$ssh = (Get-Command ssh -CommandType Application -ErrorAction Stop).Source
$scp = (Get-Command scp -CommandType Application -ErrorAction Stop).Source
$target = "$SshUser@$SshHost"
$publishNonce = [guid]::NewGuid().ToString('N')
$stage = "/tmp/ayla-release-$normalizedVersion-$publishNonce"
$remotePublisherPath = Join-Path $projectRoot 'scripts\Publish-AylaSelfHostedRelease.remote.sh'

try {
    Invoke-CheckedCommand -Executable $ssh -Operation 'Create remote staging directory' -Arguments @(
        '-o', 'BatchMode=yes', '-i', $IdentityFile, '-p', [string]$SshPort,
        $target, "install -d -m 0700 '$stage'"
    )
    Invoke-CheckedCommand -Executable $scp -Operation 'Upload signed release artifacts' -Arguments @(
        '-o', 'BatchMode=yes', '-i', $IdentityFile, '-P', [string]$SshPort,
        $installerPath, $signaturePath, $metadataPath, $remotePublisherPath, "${target}:$stage/"
    )

    $remotePublish = "sudo /bin/sh '$stage/Publish-AylaSelfHostedRelease.remote.sh' " +
        "'$stage' '$normalizedVersion' '$installerHash' '$installerSize' '$publishNonce'"
    Invoke-CheckedCommand -Executable $ssh -Operation 'Atomically publish release' -Arguments @(
        '-o', 'BatchMode=yes', '-i', $IdentityFile, '-p', [string]$SshPort,
        $target, $remotePublish
    )
}
finally {
    & $ssh -o BatchMode=yes -i $IdentityFile -p $SshPort $target "rm -rf -- '$stage'" 2>$null
}

$publicMetadata = Invoke-RestMethod -Uri "$latestUrl`?verify=$([guid]::NewGuid().ToString('N'))" `
    -Headers @{ 'Cache-Control' = 'no-cache'; 'User-Agent' = 'AylaSelfHostedPublisher/1.0' } `
    -TimeoutSec 30
if ([string]$publicMetadata.version -ne $normalizedVersion -or
    [string]$publicMetadata.platforms.$platform.signature -ne $signature -or
    [string]$publicMetadata.platforms.$platform.url -ne $immutableInstallerUrl) {
    throw 'The public latest.json does not match the release that was published.'
}

$downloadPath = [System.IO.Path]::GetTempFileName()
try {
    Invoke-WebRequest -Uri $immutableInstallerUrl -OutFile $downloadPath -TimeoutSec 120 -UseBasicParsing
    $download = Get-Item -LiteralPath $downloadPath
    $downloadHash = (Get-FileHash -LiteralPath $downloadPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($download.Length -ne $installerSize -or $downloadHash -ne $installerHash) {
        throw 'The public installer failed size or SHA-256 verification.'
    }
}
finally {
    [System.IO.File]::Delete($downloadPath)
}

Write-Output "Published and verified Ayla $normalizedVersion on the self-hosted update channel."
