$ErrorActionPreference = "Stop"
$tar = Join-Path $env:SystemRoot "System32\tar.exe"

$repoRoot = Split-Path -Parent $PSScriptRoot
$vendorRoot = Join-Path $repoRoot ".cargo\rune\vendor"
$revision = "ff8c5bfa1aa2458219c0a5e91ebc98cb34400e39"
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ai-shorts-rune-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $tempRoot | Out-Null

function Copy-Tree {
    param([string]$Source, [string]$Destination)
    New-Item -ItemType Directory -Force -Path $Destination | Out-Null
    Get-ChildItem -LiteralPath $Source -Force | ForEach-Object {
        if ($_.Name -eq ".git") { return }
        $target = Join-Path $Destination $_.Name
        if ($_.PSIsContainer) {
            Copy-Tree -Source $_.FullName -Destination $target
        } else {
            Copy-Item -LiteralPath $_.FullName -Destination $target -Force
        }
    }
}

try {
    $archive = Join-Path $tempRoot "rune.tar.gz"
    $unpacked = Join-Path $tempRoot "rune"
    New-Item -ItemType Directory -Force -Path $unpacked | Out-Null
    Invoke-WebRequest -Uri "https://github.com/anomalyco/rune/archive/$revision.tar.gz" -OutFile $archive
    & $tar -xzf $archive -C $unpacked
    if ($LASTEXITCODE -ne 0) { throw "Could not extract Rune archive" }
    $runeRoot = Get-ChildItem -Path $unpacked -Directory | Select-Object -First 1
    if ($null -eq $runeRoot) { throw "Could not extract Rune archive" }

    $manifestPath = Join-Path $runeRoot.FullName "Cargo.toml"
    $manifest = Get-Content -Raw $manifestPath
    $section = [regex]::Match($manifest, '(?ms)\[patch\.crates-io\]\s*(.*?)(?=\n\[|\z)')
    if (-not $section.Success) { throw "Rune manifest has no patch section" }
    $vendors = @()
    foreach ($line in ($section.Groups[1].Value -split "`r?`n")) {
        $entry = [regex]::Match($line.Trim(), '^([\w-]+)\s*=\s*\{\s*path\s*=\s*"([^"]+)"\s*\}')
        if ($entry.Success) {
            $vendors += @{ Crate = $entry.Groups[1].Value; Source = $entry.Groups[2].Value }
        }
    }
    if ($vendors.Count -eq 0) { throw "No vendor crates found in Rune manifest" }

    New-Item -ItemType Directory -Force -Path $vendorRoot | Out-Null
    $patchLines = @("[patch.crates-io]")
    foreach ($vendor in $vendors) {
        $name = $vendor.Crate
        $source = Join-Path $runeRoot.FullName $vendor.Source
        $destination = Join-Path $vendorRoot $name
        if (-not (Test-Path (Join-Path $source "Cargo.toml"))) {
            throw "Vendor crate $name was not found at $($vendor.Source)"
        }
        if (Test-Path $destination) {
            Remove-Item -Recurse -Force $destination
        }
        Copy-Tree -Source $source -Destination $destination
        $patchLines += "$name = { path = `".cargo/rune/vendor/$name`" }"
        Write-Host "Vendored $name"
    }
    Set-Content -Path (Join-Path $repoRoot ".cargo\rune\patches.toml") -Value $patchLines
    Write-Host "Rune vendor patches are ready."
} finally {
    Remove-Item -Recurse -Force $tempRoot
}
