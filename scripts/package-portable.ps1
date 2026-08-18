# package-portable.ps1
#
# Builds a release GameTrimmer and packs the portable zip:
#   GameTrimmer-<version>-portable-win64.zip
#   `-- GameTrimmer-<version>/
#       |-- gametrimmer.exe   (release, strip=true, with the icon and the DPI
#       |                      manifest)
#       |-- rules.json        (shipped so the first run needs no write access
#       |                      to materialize it - ensure_rules_path() would
#       |                      create it anyway if the file were missing)
#       |-- l10n_rules.json   (the same, for the localization rules)
#       |-- README.md         (English - what most people who downloaded a
#       |                      release zip read)
#       |-- README.uk.md      (Ukrainian; both files ship, because inside an
#       |                      archive there is no link to follow between them)
#       |-- BUILDINFO.txt     (version, source SHA/ref/tag and rustc version)
#       |-- LICENSE
#       `-- THIRD-PARTY-NOTICES.md  (the MIT licence of TikiOne Steam Cleaner,
#                                    whose redistributable list seeded these
#                                    rules - MIT requires its text to travel
#                                    with copies)
#
# The path to the built exe comes from `cargo metadata` rather than
# `target/release/...` directly: target-dir does not have to live inside the
# project. CARGO_TARGET_DIR moves it, so does `build.target-dir` in any of
# Cargo's config files, and so does a change in workspace layout.
# `cargo metadata` answers from the same source the build itself reads, so the
# two cannot drift apart.
#
# Historical note: a `.cargo/config.toml` did once live here, moving target-dir
# outside the project because autocfg tripped over the Cyrillic path. That
# workaround was removed on 2026-07-24 (6814264), and target-dir is a plain
# `target/` in the repository root today. `cargo metadata` stays for the reason
# above, not for that one.
#
# Run it (from the repository root or from anywhere):
#   pwsh -File scripts\package-portable.ps1
#
# The finished zip appears at dist\GameTrimmer-<version>-portable-win64.zip.

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    # 1. Version from Cargo.toml (workspace.package.version)
    $cargoToml = Get-Content "$repoRoot\Cargo.toml" -Raw
    if ($cargoToml -notmatch 'version\s*=\s*"([^"]+)"') {
        throw "could not read the version from Cargo.toml"
    }
    $version = $Matches[1]

    # 2. Release build
    cargo build --release -p gametrimmer -p gametrimmer-watch --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release failed" }

    # 3. The real target-dir, as the build itself sees it
    $metadata = cargo metadata --format-version 1 --no-deps --locked | ConvertFrom-Json
    $targetDir = $metadata.target_directory
    $exePath = Join-Path $targetDir "release\gametrimmer.exe"
    $watchExePath = Join-Path $targetDir "release\gametrimmer-watch.exe"
    if (-not (Test-Path $exePath)) {
        throw "no exe at $exePath"
    }
    if (-not (Test-Path $watchExePath)) {
        throw "no exe at $watchExePath"
    }

    # 4. Assemble the package contents
    $distDir = Join-Path $repoRoot "dist"
    $stageDir = Join-Path $distDir "GameTrimmer-$version"
    $resolvedDist = [IO.Path]::GetFullPath($distDir).TrimEnd([IO.Path]::DirectorySeparatorChar)
    $resolvedStage = [IO.Path]::GetFullPath($stageDir)
    if (-not $resolvedStage.StartsWith($resolvedDist + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to clean a stage directory outside dist: $resolvedStage"
    }
    # The directory's contents, not the directory itself: while an Explorer
    # window or a shell sits inside it, Windows refuses to REMOVE the directory
    # but happily lets files be created in it. Removing it outright failed here
    # with "process cannot access the file" and killed the packaging run after
    # a successful build.
    if (Test-Path $stageDir) {
        Get-ChildItem -Path $stageDir -Force -Exclude "*.db*", "*.log*" | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
    }
    New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

    Copy-Item $exePath (Join-Path $stageDir "gametrimmer.exe") -Force
    Copy-Item $watchExePath (Join-Path $stageDir "gametrimmer-watch.exe") -Force
    Copy-Item "$repoRoot\rules.json" (Join-Path $stageDir "rules.json") -Force
    Copy-Item "$repoRoot\l10n_rules.json" (Join-Path $stageDir "l10n_rules.json") -Force
    Copy-Item "$repoRoot\locales" (Join-Path $stageDir "locales") -Recurse -Force
    Copy-Item "$repoRoot\README.md" (Join-Path $stageDir "README.md") -Force
    Copy-Item "$repoRoot\README.uk.md" (Join-Path $stageDir "README.uk.md") -Force
    Copy-Item "$repoRoot\LICENSE" (Join-Path $stageDir "LICENSE") -Force
    Copy-Item "$repoRoot\THIRD-PARTY-NOTICES.md" (Join-Path $stageDir "THIRD-PARTY-NOTICES.md") -Force

    $sourceCommit = (git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw "could not resolve the source commit" }
    $sourceTag = (git tag --points-at HEAD | Select-Object -First 1)
    $sourceRef = if ($env:GITHUB_REF_NAME) { $env:GITHUB_REF_NAME } else { (git branch --show-current).Trim() }
    if (-not $sourceRef) { $sourceRef = $sourceCommit }
    $sourceDirty = [bool](git status --porcelain --untracked-files=normal)
    $rustcVersion = (rustc --version).Trim()
    $buildInfo = @(
        "version=$version"
        "source_commit=$sourceCommit"
        "source_ref=$sourceRef"
        "source_tag=$sourceTag"
        "source_dirty=$($sourceDirty.ToString().ToLowerInvariant())"
        "rustc=$rustcVersion"
    ) -join "`n"
    [IO.File]::WriteAllText(
        (Join-Path $stageDir "BUILDINFO.txt"),
        $buildInfo + "`n",
        [Text.UTF8Encoding]::new($false))

    # 5. Zip - through .NET ZipFile rather than Compress-Archive
    $zipTempDir = Join-Path $env:TEMP "GameTrimmer-ZipStage-$PID"
    if (Test-Path $zipTempDir) { Remove-Item $zipTempDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $zipTempDir | Out-Null
    try {
        Copy-Item $exePath (Join-Path $zipTempDir "gametrimmer.exe")
        Copy-Item $watchExePath (Join-Path $zipTempDir "gametrimmer-watch.exe")
        Copy-Item "$repoRoot\rules.json" (Join-Path $zipTempDir "rules.json")
        Copy-Item "$repoRoot\l10n_rules.json" (Join-Path $zipTempDir "l10n_rules.json")
        Copy-Item "$repoRoot\locales" (Join-Path $zipTempDir "locales") -Recurse
        Copy-Item "$repoRoot\README.md" (Join-Path $zipTempDir "README.md")
        Copy-Item "$repoRoot\README.uk.md" (Join-Path $zipTempDir "README.uk.md")
        Copy-Item "$repoRoot\LICENSE" (Join-Path $zipTempDir "LICENSE")
        Copy-Item "$repoRoot\THIRD-PARTY-NOTICES.md" (Join-Path $zipTempDir "THIRD-PARTY-NOTICES.md")
        Copy-Item (Join-Path $stageDir "BUILDINFO.txt") (Join-Path $zipTempDir "BUILDINFO.txt")

        $zipPath = Join-Path $distDir "GameTrimmer-$version-portable-win64.zip"
        if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        [System.IO.Compression.ZipFile]::CreateFromDirectory(
            $zipTempDir,
            $zipPath,
            [System.IO.Compression.CompressionLevel]::SmallestSize,
            $false)
    } finally {
        if (Test-Path $zipTempDir) { Remove-Item $zipTempDir -Recurse -Force -ErrorAction SilentlyContinue }
    }

    $exeSizeMb = [Math]::Round((Get-Item $exePath).Length / 1MB, 2)
    $watchSizeMb = [Math]::Round((Get-Item $watchExePath).Length / 1MB, 2)
    $zipSizeMb = [Math]::Round((Get-Item $zipPath).Length / 1MB, 2)
    Write-Host "Done: $zipPath"
    Write-Host "  gametrimmer.exe: $exeSizeMb MB, gametrimmer-watch.exe: $watchSizeMb MB, zip: $zipSizeMb MB"
}
finally {
    Pop-Location
}
