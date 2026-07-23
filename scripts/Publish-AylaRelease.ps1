#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [string]$ArtifactDirectory,

    [string]$Notes,

    [switch]$DryRun,

    [switch]$ResumeDraft
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (Test-Path Env:GH_DEBUG) {
    throw 'GH_DEBUG must be unset for release publication so diagnostic output cannot expose sensitive context.'
}

if ($DryRun -and $ResumeDraft) {
    throw 'ResumeDraft cannot be combined with DryRun because resuming requires an existing GitHub draft.'
}

$repository = 'sf0rzin/ayla-releases'
$platform = 'windows-x86_64'
$projectRoot = Split-Path -Parent $PSScriptRoot
$publicLatestUrl = "https://github.com/$repository/releases/latest/download/latest.json"
$githubLatestApiUrl = "https://api.github.com/repos/$repository/releases/latest"

function ConvertTo-StableSemVer {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value,

        [Parameter(Mandatory = $true)]
        [string]$Source
    )

    $match = [regex]::Match(
        $Value.Trim(),
        '^(?:v)?(?<normalized>(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*))$',
        [System.Text.RegularExpressions.RegexOptions]::CultureInvariant
    )

    if (-not $match.Success) {
        throw "$Source must be a stable SemVer value such as '0.2.0'."
    }

    return $match.Groups['normalized'].Value
}

function Compare-StableSemVer {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Left,

        [Parameter(Mandatory = $true)]
        [string]$Right
    )

    try {
        $leftParts = @($Left.Split('.') | ForEach-Object { [uint64]::Parse($_) })
        $rightParts = @($Right.Split('.') | ForEach-Object { [uint64]::Parse($_) })
    }
    catch {
        throw 'SemVer components are too large to compare safely.'
    }

    for ($index = 0; $index -lt 3; $index++) {
        if ($leftParts[$index] -lt $rightParts[$index]) {
            return -1
        }
        if ($leftParts[$index] -gt $rightParts[$index]) {
            return 1
        }
    }

    return 0
}

function Get-CargoPackageVersion {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $insidePackage = $false
    $versions = @()

    foreach ($line in Get-Content -LiteralPath $Path) {
        if ($line -match '^\s*\[(?<section>[^]]+)\]\s*$') {
            if ($insidePackage) {
                break
            }
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

function Invoke-CapturedCommand {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Executable,

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,

        [Parameter(Mandatory = $true)]
        [string]$Operation
    )

    $stdoutPath = [System.IO.Path]::GetTempFileName()
    $stderrPath = [System.IO.Path]::GetTempFileName()

    try {
        & $Executable @Arguments 1> $stdoutPath 2> $stderrPath
        $exitCode = $LASTEXITCODE
        $stdout = [System.IO.File]::ReadAllText($stdoutPath)
        $hadStderr = (Get-Item -LiteralPath $stderrPath).Length -gt 0

        if ($exitCode -ne 0) {
            throw "$Operation failed with exit code $exitCode. Diagnostic output was suppressed."
        }

        return [pscustomobject]@{
            StdOut = $stdout
            HadStderr = $hadStderr
        }
    }
    finally {
        [System.IO.File]::Delete($stdoutPath)
        [System.IO.File]::Delete($stderrPath)
    }
}

function Invoke-GitHubCli {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Operation,

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    if (Test-Path Env:GH_DEBUG) {
        throw 'GH_DEBUG became set during the release process; refusing to invoke GitHub CLI.'
    }

    $gh = Get-Command gh -CommandType Application -ErrorAction SilentlyContinue
    if (-not $gh) {
        throw "GitHub CLI ('gh') is required for release publication."
    }

    return Invoke-CapturedCommand -Executable $gh.Source -Arguments $Arguments -Operation $Operation
}

function Invoke-GitHubJson {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Operation,

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    $result = Invoke-GitHubCli -Operation $Operation -Arguments $Arguments
    if ([string]::IsNullOrWhiteSpace([string]$result.StdOut)) {
        throw "$Operation returned an empty JSON response."
    }

    try {
        $parsed = ConvertFrom-Json -InputObject $result.StdOut
        if ($parsed -is [System.Array]) {
            foreach ($item in $parsed) {
                Write-Output $item
            }
            return
        }

        return $parsed
    }
    catch {
        throw "$Operation returned invalid JSON on stdout. GitHub CLI stderr remained isolated and was suppressed."
    }
}

function Get-ResponseStatusCode {
    param(
        [Parameter(Mandatory = $true)]
        [System.Management.Automation.ErrorRecord]$ErrorRecord
    )

    $response = $null
    if ($ErrorRecord.Exception.PSObject.Properties.Name -contains 'Response') {
        $response = $ErrorRecord.Exception.Response
    }
    if (-not $response -and $ErrorRecord.Exception.InnerException -and
        $ErrorRecord.Exception.InnerException.PSObject.Properties.Name -contains 'Response') {
        $response = $ErrorRecord.Exception.InnerException.Response
    }

    if (-not $response) {
        return $null
    }

    try {
        return [int]$response.StatusCode
    }
    catch {
        return $null
    }
}

function Invoke-AnonymousWebRequest {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Uri,

        [string]$OutFile
    )

    $parameters = @{
        Uri = $Uri
        Headers = @{
            'Accept' = 'application/vnd.github+json'
            'Cache-Control' = 'no-cache'
            'User-Agent' = 'AylaReleasePublisher/1.0'
            'X-GitHub-Api-Version' = '2022-11-28'
        }
        MaximumRedirection = 8
        TimeoutSec = 60
        UseBasicParsing = $true
    }

    if (-not [string]::IsNullOrWhiteSpace($OutFile)) {
        $parameters['OutFile'] = $OutFile
    }

    return Invoke-WebRequest @parameters
}

function Convert-WebResponseContentToText {
    param(
        [Parameter(Mandatory = $true)]
        $Response
    )

    if ($Response.Content -is [byte[]]) {
        return [System.Text.Encoding]::UTF8.GetString($Response.Content)
    }

    return [string]$Response.Content
}

function Get-LatestPublishedVersion {
    try {
        $response = Invoke-AnonymousWebRequest -Uri $githubLatestApiUrl
    }
    catch {
        if ((Get-ResponseStatusCode -ErrorRecord $_) -eq 404) {
            return $null
        }
        throw "Unable to read the current public GitHub release without authentication."
    }

    try {
        $release = ConvertFrom-Json -InputObject (Convert-WebResponseContentToText -Response $response)
    }
    catch {
        throw 'The current public GitHub release response is not valid JSON.'
    }

    if ([bool]$release.draft -or [bool]$release.prerelease) {
        throw 'GitHub latest unexpectedly returned a draft or prerelease.'
    }

    return ConvertTo-StableSemVer -Value ([string]$release.tag_name) -Source 'Current published release tag'
}

function Get-ReleaseState {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ReleaseTag
    )

    return Invoke-GitHubJson -Operation "Read release $ReleaseTag" -Arguments @(
        'release', 'view', $ReleaseTag,
        '--repo', $repository,
        '--json', 'tagName,isDraft,url,assets'
    )
}

function Assert-RemoteAssets {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Release,

        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$ExpectedAssets,

        [Parameter(Mandatory = $true)]
        [bool]$ExpectedDraft,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedTag
    )

    if (-not [string]::Equals([string]$Release.tagName, $ExpectedTag, [System.StringComparison]::Ordinal)) {
        throw "Release tag mismatch: expected '$ExpectedTag'."
    }

    if ([bool]$Release.isDraft -ne $ExpectedDraft) {
        throw "Release '$ExpectedTag' has an unexpected draft state."
    }

    $remoteAssets = @($Release.assets)
    if ($remoteAssets.Count -ne $ExpectedAssets.Count) {
        throw "Release '$ExpectedTag' must contain exactly $($ExpectedAssets.Count) assets; GitHub reports $($remoteAssets.Count)."
    }

    foreach ($entry in $ExpectedAssets.GetEnumerator()) {
        $assetName = [string]$entry.Key
        $expected = $entry.Value
        $matches = @($remoteAssets | Where-Object {
            [string]::Equals([string]$_.name, $assetName, [System.StringComparison]::Ordinal)
        })

        if ($matches.Count -ne 1) {
            throw "Release '$ExpectedTag' does not contain exactly one asset named '$assetName'."
        }

        $remote = $matches[0]
        if ([string]$remote.state -ne 'uploaded') {
            throw "Asset '$assetName' is not fully uploaded."
        }

        if ([long]$remote.size -ne [long]$expected.Size) {
            throw "Asset '$assetName' size does not match the local file."
        }

        if ($remote.PSObject.Properties.Name -contains 'digest' -and
            -not [string]::IsNullOrWhiteSpace([string]$remote.digest)) {
            $expectedDigest = "sha256:$($expected.Sha256)"
            if (-not [string]::Equals(
                    [string]$remote.digest,
                    $expectedDigest,
                    [System.StringComparison]::OrdinalIgnoreCase
                )) {
                throw "Asset '$assetName' SHA-256 digest does not match GitHub's digest."
            }
        }
    }
}

function Assert-DownloadedAssets {
    param(
        [Parameter(Mandatory = $true)]
        [System.Collections.IDictionary]$ExpectedAssets,

        [Parameter(Mandatory = $true)]
        [string]$ReleaseTag
    )

    $tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
    $verificationDirectory = Join-Path $tempRoot ("ayla-release-verify-" + [guid]::NewGuid().ToString('N'))
    $verificationDirectory = [System.IO.Path]::GetFullPath($verificationDirectory)

    if (-not $verificationDirectory.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw 'Refusing to create a verification directory outside the system temporary directory.'
    }

    New-Item -ItemType Directory -Path $verificationDirectory | Out-Null

    try {
        Invoke-GitHubCli -Operation "Download release $ReleaseTag for verification" -Arguments @(
            'release', 'download', $ReleaseTag,
            '--repo', $repository,
            '--dir', $verificationDirectory
        ) | Out-Null

        $downloadedFiles = @(Get-ChildItem -LiteralPath $verificationDirectory -File -Force)
        if ($downloadedFiles.Count -ne $ExpectedAssets.Count) {
            throw "Downloaded release '$ReleaseTag' does not contain the expected asset count."
        }

        foreach ($entry in $ExpectedAssets.GetEnumerator()) {
            $assetName = [string]$entry.Key
            $expected = $entry.Value
            $downloadedPath = Join-Path $verificationDirectory $assetName

            if (-not (Test-Path -LiteralPath $downloadedPath -PathType Leaf)) {
                throw "Downloaded release '$ReleaseTag' is missing '$assetName'."
            }

            $downloaded = Get-Item -LiteralPath $downloadedPath
            $downloadedHash = (Get-FileHash -LiteralPath $downloadedPath -Algorithm SHA256).Hash.ToLowerInvariant()
            if ([long]$downloaded.Length -ne [long]$expected.Size -or
                -not [string]::Equals(
                    $downloadedHash,
                    [string]$expected.Sha256,
                    [System.StringComparison]::OrdinalIgnoreCase
                )) {
                throw "Downloaded asset '$assetName' failed local size or SHA-256 verification."
            }
        }
    }
    finally {
        if (Test-Path -LiteralPath $verificationDirectory) {
            $resolved = [System.IO.Path]::GetFullPath($verificationDirectory)
            if ($resolved.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase)) {
                Remove-Item -LiteralPath $resolved -Recurse -Force
            }
        }
    }
}

function Assert-PublicChannelOnce {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExpectedVersion,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedSignature,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedInstallerUrl,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedInstallerHash,

        [Parameter(Mandatory = $true)]
        [long]$ExpectedInstallerSize
    )

    $cacheBuster = [guid]::NewGuid().ToString('N')
    $response = Invoke-AnonymousWebRequest -Uri "$publicLatestUrl`?verification=$cacheBuster"

    try {
        $publicMetadata = ConvertFrom-Json -InputObject (Convert-WebResponseContentToText -Response $response)
    }
    catch {
        throw 'Public latest.json is not valid JSON.'
    }

    if (-not [string]::Equals([string]$publicMetadata.version, $ExpectedVersion, [System.StringComparison]::Ordinal) -or
        -not [string]::Equals(
            [string]$publicMetadata.platforms.$platform.signature,
            $ExpectedSignature,
            [System.StringComparison]::Ordinal
        ) -or
        -not [string]::Equals(
            [string]$publicMetadata.platforms.$platform.url,
            $ExpectedInstallerUrl,
            [System.StringComparison]::Ordinal
        )) {
        throw 'Public latest.json does not match the version, signature, and immutable installer URL that were published.'
    }

    $downloadPath = [System.IO.Path]::GetTempFileName()
    try {
        Invoke-AnonymousWebRequest -Uri $ExpectedInstallerUrl -OutFile $downloadPath | Out-Null
        $downloaded = Get-Item -LiteralPath $downloadPath
        $downloadedHash = (Get-FileHash -LiteralPath $downloadPath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ([long]$downloaded.Length -ne $ExpectedInstallerSize -or
            -not [string]::Equals(
                $downloadedHash,
                $ExpectedInstallerHash,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw 'The anonymously downloaded public installer failed size or SHA-256 verification.'
        }
    }
    finally {
        [System.IO.File]::Delete($downloadPath)
    }
}

function Assert-PublicChannel {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ExpectedVersion,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedSignature,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedInstallerUrl,

        [Parameter(Mandatory = $true)]
        [string]$ExpectedInstallerHash,

        [Parameter(Mandatory = $true)]
        [long]$ExpectedInstallerSize
    )

    $lastFailure = $null
    for ($attempt = 1; $attempt -le 6; $attempt++) {
        try {
            Assert-PublicChannelOnce @PSBoundParameters
            return
        }
        catch {
            $lastFailure = $_.Exception.Message
            if ($attempt -lt 6) {
                Start-Sleep -Seconds 5
            }
        }
    }

    throw "Release was published, but anonymous public-channel verification remained inconsistent after 30 seconds: $lastFailure Follow the quarantine procedure in docs/forge-build.md."
}

$normalizedVersion = ConvertTo-StableSemVer -Value $Version -Source 'Requested version'
$tag = "v$normalizedVersion"
$expectedInstallerName = "Ayla_${normalizedVersion}_x64-setup.exe"
$expectedSignatureName = "$expectedInstallerName.sig"

if ([string]::IsNullOrWhiteSpace($Notes)) {
    $Notes = "Ayla $normalizedVersion Windows update."
}

$packagePath = Join-Path $projectRoot 'package.json'
$tauriConfigPath = Join-Path $projectRoot 'src-tauri\tauri.conf.json'
$cargoManifestPath = Join-Path $projectRoot 'src-tauri\Cargo.toml'
$verifierManifestPath = Join-Path $projectRoot 'tools\update-verifier\Cargo.toml'

$packageJson = Get-Content -LiteralPath $packagePath -Raw | ConvertFrom-Json
$tauriConfig = Get-Content -LiteralPath $tauriConfigPath -Raw | ConvertFrom-Json
$manifestVersions = [ordered]@{
    'package.json' = ConvertTo-StableSemVer -Value ([string]$packageJson.version) -Source 'package.json version'
    'src-tauri/tauri.conf.json' = ConvertTo-StableSemVer -Value ([string]$tauriConfig.version) -Source 'tauri.conf.json version'
    'src-tauri/Cargo.toml' = ConvertTo-StableSemVer -Value (Get-CargoPackageVersion -Path $cargoManifestPath) -Source 'Cargo.toml package version'
}

foreach ($entry in $manifestVersions.GetEnumerator()) {
    if (-not [string]::Equals([string]$entry.Value, $normalizedVersion, [System.StringComparison]::Ordinal)) {
        throw "Requested version '$normalizedVersion' does not match $($entry.Key) version '$($entry.Value)'."
    }
}

$wrappedPublicKey = [string]$tauriConfig.plugins.updater.pubkey
if ([string]::IsNullOrWhiteSpace($wrappedPublicKey)) {
    throw 'src-tauri/tauri.conf.json does not contain an updater public key.'
}

$artifactItem = Get-Item -LiteralPath $ArtifactDirectory -Force
if (-not $artifactItem.PSIsContainer) {
    throw "ArtifactDirectory must point to a directory: '$ArtifactDirectory'."
}

$artifactPath = $artifactItem.FullName
$installers = @(Get-ChildItem -LiteralPath $artifactPath -File -Force | Where-Object {
    [string]::Equals($_.Name, $expectedInstallerName, [System.StringComparison]::Ordinal)
})
if ($installers.Count -ne 1) {
    throw "Artifact directory must contain exactly one '$expectedInstallerName' file; found $($installers.Count)."
}

$installer = $installers[0]

$signatures = @(Get-ChildItem -LiteralPath $artifactPath -File -Force | Where-Object {
    [string]::Equals($_.Name, $expectedSignatureName, [System.StringComparison]::Ordinal)
})
if ($signatures.Count -ne 1) {
    throw "Artifact directory must contain exactly one '$expectedSignatureName' file; found $($signatures.Count)."
}

$signatureFile = $signatures[0]

foreach ($artifact in @($installer, $signatureFile)) {
    if (($artifact.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Release artifact '$($artifact.Name)' must not be a symbolic link or reparse point."
    }
    if ($artifact.Length -le 0) {
        throw "Release artifact '$($artifact.Name)' is empty."
    }
}

$productVersion = [string]$installer.VersionInfo.ProductVersion
if ([string]::IsNullOrWhiteSpace($productVersion)) {
    throw 'The NSIS installer does not expose ProductVersion; refusing an artifact that cannot be bound to the requested release version.'
}
$normalizedProductVersion = ConvertTo-StableSemVer -Value $productVersion -Source 'NSIS ProductVersion'
if (-not [string]::Equals($normalizedProductVersion, $normalizedVersion, [System.StringComparison]::Ordinal)) {
    throw "NSIS ProductVersion '$normalizedProductVersion' does not match requested version '$normalizedVersion'."
}

$cargo = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue
if (-not $cargo) {
    throw "Rust Cargo is required to run the locked updater signature verifier."
}

$verificationResult = Invoke-CapturedCommand -Executable $cargo.Source -Operation 'Cryptographically verify updater signature' -Arguments @(
    'run', '--quiet', '--locked',
    '--manifest-path', $verifierManifestPath,
    '--',
    $installer.FullName,
    $signatureFile.FullName,
    $wrappedPublicKey
)
if (-not [string]::Equals(
        ([string]$verificationResult.StdOut).Trim(),
        'signature verified',
        [System.StringComparison]::Ordinal
    )) {
    throw 'The locked updater verifier returned an unexpected response.'
}

$currentPublishedVersion = Get-LatestPublishedVersion
if ($null -ne $currentPublishedVersion -and
    (Compare-StableSemVer -Left $normalizedVersion -Right $currentPublishedVersion) -le 0) {
    throw "Requested version '$normalizedVersion' must be strictly greater than current published version '$currentPublishedVersion'."
}

$signature = [System.IO.File]::ReadAllText($signatureFile.FullName)
if ([string]::IsNullOrWhiteSpace($signature)) {
    throw "Signature file '$expectedSignatureName' does not contain a signature."
}

$immutableInstallerUrl = "https://github.com/$repository/releases/download/$tag/$expectedInstallerName"
$publishedAt = [DateTimeOffset]::UtcNow.ToString(
    "yyyy-MM-dd'T'HH:mm:ss'Z'",
    [System.Globalization.CultureInfo]::InvariantCulture
)
$metadata = [ordered]@{
    version = $normalizedVersion
    notes = $Notes
    pub_date = $publishedAt
    platforms = [ordered]@{
        $platform = [ordered]@{
            signature = $signature
            url = $immutableInstallerUrl
        }
    }
}

$metadataPath = Join-Path $artifactPath 'latest.json'
if (Test-Path -LiteralPath $metadataPath) {
    $existingMetadata = Get-Item -LiteralPath $metadataPath -Force
    if (($existingMetadata.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to replace reparse point '$metadataPath'."
    }
}

$metadataJson = ConvertTo-Json -InputObject $metadata -Depth 6
$utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($metadataPath, $metadataJson + [Environment]::NewLine, $utf8WithoutBom)

$assetPaths = @($installer.FullName, $signatureFile.FullName, $metadataPath)
$expectedAssets = [ordered]@{}
foreach ($assetPath in $assetPaths) {
    $asset = Get-Item -LiteralPath $assetPath -Force
    $expectedAssets[$asset.Name] = [pscustomobject]@{
        Size = [long]$asset.Length
        Sha256 = (Get-FileHash -LiteralPath $asset.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    }
}

$installerHash = [string]$expectedAssets[$expectedInstallerName].Sha256
$releaseNotes = "$Notes`n`nInstaller SHA-256: ``$installerHash``"
Write-Output "Prepared and cryptographically verified Ayla $normalizedVersion release metadata."
Write-Output "Installer: $($installer.FullName)"
Write-Output "Installer SHA-256: $installerHash"
Write-Output "Metadata: $metadataPath"
Write-Output "Immutable update URL: $immutableInstallerUrl"
if ($null -eq $currentPublishedVersion) {
    Write-Output 'Current published version: none (bootstrap release).'
}
else {
    Write-Output "Current published version: $currentPublishedVersion"
}

if ($DryRun) {
    Write-Output 'Dry run complete. No GitHub release was created or modified.'
    return
}

$repositoryState = Invoke-GitHubJson -Operation "Read repository $repository" -Arguments @(
    'repo', 'view', $repository,
    '--json', 'nameWithOwner,visibility'
)
if (-not [string]::Equals(
        [string]$repositoryState.nameWithOwner,
        $repository,
        [System.StringComparison]::OrdinalIgnoreCase
    ) -or [string]$repositoryState.visibility -ne 'PUBLIC') {
    throw "Release repository '$repository' must exist and be public."
}

$existingReleases = @(Invoke-GitHubJson -Operation "List releases in $repository" -Arguments @(
    'release', 'list',
    '--repo', $repository,
    '--limit', '1000',
    '--json', 'tagName,isDraft'
))
$matchingReleases = @($existingReleases | Where-Object {
    [string]::Equals([string]$_.tagName, $tag, [System.StringComparison]::Ordinal)
})

$draftSelected = $false
$releasePublished = $false

try {
    if ($ResumeDraft) {
        if ($matchingReleases.Count -ne 1) {
            throw "ResumeDraft requires exactly one existing release with tag '$tag'."
        }

        $emptyDraft = Get-ReleaseState -ReleaseTag $tag
        if (-not [bool]$emptyDraft.isDraft -or @($emptyDraft.assets).Count -ne 0) {
            throw "ResumeDraft accepts only one empty, unpublished draft for tag '$tag'."
        }
        Invoke-GitHubCli -Operation "Refresh empty draft release $tag" -Arguments @(
            'release', 'edit', $tag,
            '--repo', $repository,
            '--title', "Ayla $normalizedVersion",
            '--notes', $releaseNotes
        ) | Out-Null
        $draftSelected = $true
    }
    else {
        if ($matchingReleases.Count -ne 0) {
            throw "Release '$tag' already exists; use ResumeDraft only if it is an empty draft."
        }

        Invoke-GitHubCli -Operation "Create draft release $tag" -Arguments @(
            'release', 'create', $tag,
            '--repo', $repository,
            '--draft',
            '--title', "Ayla $normalizedVersion",
            '--notes', $releaseNotes
        ) | Out-Null
        $draftSelected = $true

        $emptyDraft = Get-ReleaseState -ReleaseTag $tag
        if (-not [bool]$emptyDraft.isDraft -or @($emptyDraft.assets).Count -ne 0) {
            throw "New release '$tag' is not an empty draft; refusing to upload assets."
        }
    }

    # gh rejects duplicate asset names by default. Deliberately do not pass --clobber.
    Invoke-GitHubCli -Operation "Upload assets to draft release $tag" -Arguments @(
        'release', 'upload', $tag,
        $installer.FullName,
        $signatureFile.FullName,
        $metadataPath,
        '--repo', $repository
    ) | Out-Null

    $uploadedDraft = Get-ReleaseState -ReleaseTag $tag
    Assert-RemoteAssets -Release $uploadedDraft -ExpectedAssets $expectedAssets -ExpectedDraft $true -ExpectedTag $tag
    Assert-DownloadedAssets -ExpectedAssets $expectedAssets -ReleaseTag $tag

    Invoke-GitHubCli -Operation "Publish verified release $tag" -Arguments @(
        'release', 'edit', $tag,
        '--repo', $repository,
        '--draft=false',
        '--latest'
    ) | Out-Null
    $releasePublished = $true

    $publishedRelease = Get-ReleaseState -ReleaseTag $tag
    Assert-RemoteAssets -Release $publishedRelease -ExpectedAssets $expectedAssets -ExpectedDraft $false -ExpectedTag $tag
    Assert-PublicChannel `
        -ExpectedVersion $normalizedVersion `
        -ExpectedSignature $signature `
        -ExpectedInstallerUrl $immutableInstallerUrl `
        -ExpectedInstallerHash $installerHash `
        -ExpectedInstallerSize ([long]$installer.Length)

    Write-Output "Published and anonymously verified release: $($publishedRelease.url)"
}
catch {
    if ($draftSelected -and -not $releasePublished) {
        Write-Warning "Release '$tag' was not published. Inspect the draft before using ResumeDraft; non-empty drafts are intentionally refused."
    }
    throw
}
