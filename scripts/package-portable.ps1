# package-portable.ps1
#
# Збирає release-версію GameTrimmer і пакує портабельний zip:
#   GameTrimmer-<version>-portable-win64.zip
#   `-- GameTrimmer-<version>/
#       |-- gametrimmer.exe   (release, strip=true, з іконкою й DPI-маніфестом)
#       |-- rules.json        (щоб перший запуск не потребував прав на запис
#       |                      для матеріалізації - ensure_rules_path()
#       |                      усе одно її створить, якщо файл відсутній)
#       |-- l10n_rules.json   (те саме для мовних правил)
#       |-- README.md
#       `-- LICENSE
#
# Шлях до зібраного exe обчислюється через `cargo metadata`, а не
# `target/release/...` напряму, бо `.cargo/config.toml` виносить target-dir
# за межі проєкту (кириличний шлях репозиторію ламає autocfg - див.
# CLAUDE.md і docs/portability-audit.md, розділ «Побічні спостереження»).
#
# Запуск (з кореня репозиторію або звідки завгодно):
#   pwsh -File scripts\package-portable.ps1
#
# Готовий zip з'являється в dist\GameTrimmer-<version>-portable-win64.zip.

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $repoRoot
try {
    # 1. Версія з Cargo.toml (workspace.package.version)
    $cargoToml = Get-Content "$repoRoot\Cargo.toml" -Raw
    if ($cargoToml -notmatch 'version\s*=\s*"([^"]+)"') {
        throw "Не вдалося визначити версію з Cargo.toml"
    }
    $version = $Matches[1]

    # 2. Release-збірка
    cargo build --release -p gametrimmer
    if ($LASTEXITCODE -ne 0) { throw "cargo build --release провалився" }

    # 3. Реальний target-dir (враховує .cargo/config.toml)
    $metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $targetDir = $metadata.target_directory
    $exePath = Join-Path $targetDir "release\gametrimmer.exe"
    if (-not (Test-Path $exePath)) {
        throw "exe не знайдено за шляхом $exePath"
    }

    # 4. Збірка вмісту пакета
    $distDir = Join-Path $repoRoot "dist"
    $stageDir = Join-Path $distDir "GameTrimmer-$version"
    if (Test-Path $stageDir) { Remove-Item $stageDir -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

    Copy-Item $exePath (Join-Path $stageDir "gametrimmer.exe")
    Copy-Item "$repoRoot\rules.json" (Join-Path $stageDir "rules.json")
    Copy-Item "$repoRoot\l10n_rules.json" (Join-Path $stageDir "l10n_rules.json")
    Copy-Item "$repoRoot\README.md" (Join-Path $stageDir "README.md")
    Copy-Item "$repoRoot\LICENSE" (Join-Path $stageDir "LICENSE")

    # 5. Zip
    $zipPath = Join-Path $distDir "GameTrimmer-$version-portable-win64.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Compress-Archive -Path "$stageDir\*" -DestinationPath $zipPath

    $exeSizeMb = [Math]::Round((Get-Item $exePath).Length / 1MB, 2)
    $zipSizeMb = [Math]::Round((Get-Item $zipPath).Length / 1MB, 2)
    Write-Host "Готово: $zipPath"
    Write-Host "  exe: $exeSizeMb МБ, zip: $zipSizeMb МБ"
}
finally {
    Pop-Location
}
