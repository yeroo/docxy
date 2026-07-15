# Fetch the generated .mpp corpus from the private github.com/yeroo/mpp-corpus
# repo into corpus/mpp/ (git-ignored here -- see corpus/mpp/.gitignore).
#
# Mirrors the docxy-corpus clone pattern documented in corpus/README.md:
# shallow-clone the separate repo to a temp dir, copy the payload in, discard
# the clone. Nothing here is needed to build or test the crates -- only
# mppread's real-file tests and manual corpus exploration use it.
#
# Usage (from anywhere):
#   corpus/tools/fetch-mpp-corpus.ps1
#
# Requires: git, with credentials that can read the PRIVATE mpp-corpus repo
# (it's plain first-party content kept private out of caution, not public).
# Anonymous HTTPS access will fail. Easiest setup: `gh auth login` once, then
# `gh auth setup-git` so plain `git clone https://...` picks up the token.

$ErrorActionPreference = "Stop"

$RepoUrl = "https://github.com/yeroo/mpp-corpus.git"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Resolve-Path (Join-Path $ScriptDir "..\..")
$DestDir = Join-Path $RepoRoot "corpus\mpp"

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Error "git is not on PATH -- install git and re-run."
    exit 1
}

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("mpp-corpus-fetch-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    Write-Host "Cloning $RepoUrl (shallow, depth 1) ..."
    $cloneDir = Join-Path $TmpDir "mpp-corpus"
    $cloneLog = & git clone --depth 1 --quiet $RepoUrl $cloneDir 2>&1
    if ($LASTEXITCODE -ne 0) {
        # Guidance must print BEFORE any error is raised: with
        # $ErrorActionPreference = "Stop", a Write-Error terminates the scope
        # and everything after it would never be seen.
        Write-Host ($cloneLog | Out-String)
        Write-Host ""
        Write-Host "error: could not clone $RepoUrl." -ForegroundColor Red
        Write-Host ""
        Write-Host "This repo is PRIVATE (first-party content, kept private out of caution)."
        Write-Host "Anonymous access will not work -- you need credentials with read access:"
        Write-Host "  gh auth login          # once, if you haven't already"
        Write-Host "  gh auth setup-git       # lets plain ``git clone https://...`` use the gh token"
        Write-Host "or configure a personal access token in your git credential store."
        exit 1
    }

    $snapshotsSrc = Join-Path $cloneDir "snapshots"
    $manifestSrc = Join-Path $cloneDir "manifest.json"
    if (-not (Test-Path $snapshotsSrc) -or -not (Test-Path $manifestSrc)) {
        Write-Error "clone succeeded but snapshots/ or manifest.json is missing -- the corpus repo layout may have changed."
        exit 1
    }

    $snapshotsDest = Join-Path $DestDir "snapshots"
    New-Item -ItemType Directory -Path $snapshotsDest -Force | Out-Null
    Copy-Item -Path (Join-Path $snapshotsSrc "*") -Destination $snapshotsDest -Recurse -Force
    Copy-Item -Path $manifestSrc -Destination (Join-Path $DestDir "manifest.json") -Force

    $fileCount = (Get-ChildItem -Path $snapshotsDest -File -Recurse | Measure-Object).Count
    $totalBytes = (Get-ChildItem -Path $snapshotsDest -File -Recurse | Measure-Object -Property Length -Sum).Sum
    $totalMb = [math]::Round($totalBytes / 1MB, 1)

    Write-Host ""
    Write-Host "Done. Copied into $DestDir`:"
    Write-Host "  snapshots/    $fileCount files ($totalMb MB)"
    Write-Host "  manifest.json"
    Write-Host ""
    Write-Host "Both stay git-ignored (see corpus/mpp/.gitignore)."
}
finally {
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
