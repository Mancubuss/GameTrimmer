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
#       |-- README.md      (англійська - мова, якою читає більшість тих, хто
#       |                   звантажив zip з релізу)
#       |-- README.uk.md   (українська; обидва файли, бо в архіві немає
#       |                   переходу за посиланням між ними)
#       |-- LICENSE
#       `-- THIRD-PARTY-NOTICES.md  (MIT-ліцензія TikiOne Steam Cleaner, чий
#                                    перелік дистрибутивів став основою правил -
#                                    MIT вимагає, щоб її текст їхав із копіями)
#
# Шлях до зібраного exe обчислюється через `cargo metadata`, а не
# `target/release/...` напряму: target-dir не обов'язково лежить у проєкті -
# його зсувають CARGO_TARGET_DIR, `build.target-dir` у будь-якому з
# конфігураційних файлів Cargo і зміна розкладки workspace. `cargo metadata`
# відповідає з того самого джерела, з якого збирає сама збірка, тож розійтися
# вони не можуть.
#
# Історична примітка: колись тут справді жив `.cargo/config.toml`, який виносив
# target-dir назовні через баг autocfg на кириличному шляху. Обхід прибрано
# 2026-07-24 (6814264), і сьогодні target-dir - звичайний `target/` у корені.
# `cargo metadata` лишається не через це, а з причини вище.
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

    # 3. Реальний target-dir, як його бачить сама збірка
    $metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    $targetDir = $metadata.target_directory
    $exePath = Join-Path $targetDir "release\gametrimmer.exe"
    if (-not (Test-Path $exePath)) {
        throw "exe не знайдено за шляхом $exePath"
    }

    # 4. Збірка вмісту пакета
    $distDir = Join-Path $repoRoot "dist"
    $stageDir = Join-Path $distDir "GameTrimmer-$version"
    # Вміст теки, а не сама тека: якщо її тримає відкрите вікно Провідника чи
    # термінал усередині, Windows забороняє ВИДАЛИТИ каталог, але дозволяє
    # створювати в ньому файли. Видалення теки цілком тут падало з "process
    # cannot access the file" і зупиняло пакування після успішної збірки.
    if (Test-Path $stageDir) {
        Get-ChildItem -Path $stageDir -Force | Remove-Item -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

    Copy-Item $exePath (Join-Path $stageDir "gametrimmer.exe")
    Copy-Item "$repoRoot\rules.json" (Join-Path $stageDir "rules.json")
    Copy-Item "$repoRoot\l10n_rules.json" (Join-Path $stageDir "l10n_rules.json")
    Copy-Item "$repoRoot\README.md" (Join-Path $stageDir "README.md")
    Copy-Item "$repoRoot\README.uk.md" (Join-Path $stageDir "README.uk.md")
    Copy-Item "$repoRoot\LICENSE" (Join-Path $stageDir "LICENSE")
    Copy-Item "$repoRoot\THIRD-PARTY-NOTICES.md" (Join-Path $stageDir "THIRD-PARTY-NOTICES.md")

    # 5. Zip - через .NET ZipFile, а не Compress-Archive: командлет не дає
    # рівня стиснення вище Optimal, тоді як SmallestSize (deflate з
    # максимальним зусиллям, .NET 6+/PowerShell 7) помітно менший для
    # великого exe. Останній аргумент $false - без кореневої теки в архіві
    # (вміст лежить у корені zip, як і в Compress-Archive "$stageDir\*").
    $zipPath = Join-Path $distDir "GameTrimmer-$version-portable-win64.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $stageDir,
        $zipPath,
        [System.IO.Compression.CompressionLevel]::SmallestSize,
        $false)

    $exeSizeMb = [Math]::Round((Get-Item $exePath).Length / 1MB, 2)
    $zipSizeMb = [Math]::Round((Get-Item $zipPath).Length / 1MB, 2)
    Write-Host "Готово: $zipPath"
    Write-Host "  exe: $exeSizeMb МБ, zip: $zipSizeMb МБ"
}
finally {
    Pop-Location
}
