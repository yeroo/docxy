# Fetch the third-party docx/xlsx corpus from the public
# github.com/yeroo/docxy-corpus repo into corpus\ (git-ignored here -- see
# .gitignore and corpus/README.md). The PowerShell twin of fetch-corpus.sh.
#
# Shallow-clones the corpus repo to a temp dir, replaces corpus\files and
# corpus\xlsx-ext with its payload (so files removed upstream go away too),
# copies the manifests, and discards the clone. Run it again to update.
#
# The round-trip fidelity gate (docs/fidelity-gate.md) reads corpus\files;
# the compare launchers and verify sweeps use the rest.
#
# Usage (from anywhere):
#   corpus/tools/fetch-corpus.ps1
#
# Offline (the clone fails), it prints a SKIP notice and exits 0, leaving any
# existing copy alone: the gate then runs the repo-tracked .docx only.

$ErrorActionPreference = "Stop"

$RepoUrl = "https://github.com/yeroo/docxy-corpus.git"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot = Resolve-Path (Join-Path $ScriptDir "..\..")
$DestDir = Join-Path $RepoRoot "corpus"

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    Write-Error "git is not on PATH -- install git and re-run."
    exit 1
}

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("docxy-corpus-fetch-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TmpDir -Force | Out-Null

try {
    Write-Host "Cloning $RepoUrl (shallow, depth 1) ..."
    $cloneDir = Join-Path $TmpDir "docxy-corpus"
    $cloneLog = & git clone --depth 1 --quiet $RepoUrl $cloneDir 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host ($cloneLog | Out-String)
        Write-Host ""
        Write-Host "SKIP: could not clone $RepoUrl (offline?). corpus\ is unchanged." -ForegroundColor Yellow
        exit 0
    }

    foreach ($dir in "files", "xlsx-ext") {
        if (-not (Test-Path (Join-Path $cloneDir $dir))) {
            Write-Error "clone succeeded but $dir/ is missing -- the corpus repo layout may have changed."
            exit 1
        }
    }

    foreach ($dir in "files", "xlsx-ext") {
        $dest = Join-Path $DestDir $dir
        if (Test-Path $dest) {
            Remove-Item -Path $dest -Recurse -Force
        }
        Copy-Item -Path (Join-Path $cloneDir $dir) -Destination $dest -Recurse -Force
    }
    Copy-Item -Path (Join-Path $cloneDir "*.json") -Destination $DestDir -Force

    $docxCount = (Get-ChildItem -Path (Join-Path $DestDir "files") -Filter *.docx -File -Recurse | Measure-Object).Count
    $xlsxCount = (Get-ChildItem -Path (Join-Path $DestDir "xlsx-ext") -Filter *.xlsx -File -Recurse | Measure-Object).Count

    Write-Host ""
    Write-Host "Done. Copied into $DestDir`:"
    Write-Host "  files\       $docxCount .docx"
    Write-Host "  xlsx-ext\    $xlsxCount .xlsx"
    Write-Host "  *.json       manifests"
}
finally {
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
