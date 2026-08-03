# Аудит портабельності GameTrimmer

Дата: 2026-07-19
Мета: програма розпаковується з zip у будь-яку теку (зокрема на флешці), не лишає слідів поза своєю текою і не залежить від встановлення.

Це документ-аудит (крок 1). Імплементація — окремим кроком. **Нічого в коді цим документом не змінено.**

> Примітка про паралельну роботу: інший агент зараз одночасно змінює
> `crates/core/src/langdetect/`, `crates/core/src/packs.rs`,
> `crates/app/src/worker/rules_io.rs` та `README.md`. Знахідки нижче, що
> стосуються цих файлів, описують їх стан **на момент аудиту** — перевірте
> актуальність перед імплементацією пунктів плану, які їх торкаються.

## Короткий підсумок

Головний, дещо несподіваний висновок: **архітектура вже майже повністю портабельна**. Автор явно про це думав заздалегідь:

- Єдина точка істини для розташування даних — `std::env::current_exe()` (не CWD). Використовується рівно у двох місцях (`crates/app/src/worker/mod.rs:112`, `crates/app/src/elevation.rs:66`), і `worker::exe_dir()` (`mod.rs:111-117`) — єдине джерело для БД, `rules.json`, `l10n_rules.json`.
- БД, `rules.json`, `l10n_rules.json` — всі поруч з exe, з graceful-деградацією при помилці (не падають, показують повідомлення користувачу).
- Усі провайдери бібліотек (`crates/core/src/providers/*.rs`) читають реєстр/AppData **лише на читання** — жодного запису чи створення ключів/файлів нема (перевірено grep'ом по всьому `providers/`).
- `eframe`/`egui` **не** використовує персистентність (`persistence`-фіча не увімкнена — підтверджено відсутністю пакетів `directories`/`ron` у `Cargo.lock`), тобто egui сам по собі нічого не пише в `%APPDATA%`.
- Немає `std::env::set_current_dir`, немає жодного хардкоду `%APPDATA%`/`%LOCALAPPDATA%` для запису.

Знахідки, які справді варто виправити перед релізом — усі дрібні (нижче в таблиці, пріоритет P1):

1. **`VACUUM` (стискання БД) не задає `PRAGMA temp_store`**, тому SQLite за замовчуванням може писати тимчасові файли в `%TEMP%` користувача (поза текою програми) під час компакції — єдине місце в коді, де стан потенційно виходить за межі теки програми (`crates/core/src/db.rs:73-87`).
2. **WAL-режим SQLite** (`crates/core/src/db.rs:75`) не має fallback'у — на екзотичних файлових системах (мережеві диски, деякі FAT32-флешки) WAL може не спрацювати; наразі це просто призведе до помилки відкриття БД замість тихого падіння в інший journal-режим.
3. **DPI-маніфест відсутній**: `crates/app/build.rs` вбудовує лише іконку, без Windows-маніфесту з `dpiAwareness`. `winit` (0.30.13, транзитивна залежність через `eframe`) сам викликає `SetProcessDpiAwarenessContext` у рантаймі, тож імовірно все й так працює per-monitor-v2 — але це **потрібно перевірити руками** (пункт 4 задачі), і варто додати явний маніфест як defense-in-depth.
4. Побічна, не зовсім портабельна, але вартна уваги знахідка: **`profile.release` в кореневому `Cargo.toml` має `panic = "abort"`**, а код у `crates/app/src/worker/scan.rs:528` і `crates/core/src/mftscan/mod.rs:151` покладається на `catch_unwind` для ізоляції паніки одного провайдера/задачі від решти сканування. З `panic = "abort"` `catch_unwind` **не спрацює** в release-збірці — паніка завжди вб'є весь процес. Це не стосується файлової портабельності напряму, але впливає на стійкість (і опосередковано — на "чи з'явиться дамп"), тож фіксую тут окремо.

## Таблиця знахідок

Умовні позначення: ✅ портабельно як є · ⚠️ портабельно, але крихко/потребує перевірки · ❌ треба виправити.

| # | Місце в коді | Що робить | Портабельно? | Що змінити |
|---|---|---|---|---|
| 1 | `crates/app/src/worker/mod.rs:111-117` (`exe_dir`) | Визначає теку даних через `std::env::current_exe()` | ✅ | Нічого — це і є правильний патерн, вся решта на ньому базується |
| 2 | `crates/app/src/worker/mod.rs:120-122` (`db_path`) | `gametrimmer.db` поруч з exe | ✅ | — |
| 3 | `crates/app/src/worker/mod.rs:140-146`, `:151-156` (`ensure_rules_path`, `ensure_l10n_rules_path`) | Матеріалізує `rules.json`/`l10n_rules.json` поруч з exe з вбудованих дефолтів при першому запуску, якщо файл відсутній (`ensure_data_file_in`, `mod.rs:127-133`) | ✅ | — |
| 4 | `crates/core/src/rules.rs:22-23` | `BUILTIN_RULES_JSON` — `include_str!` кореневого `rules.json` **на етапі компіляції** | ✅ | — (компайл-тайм, не рантайм; просто контекст для п. 3) |
| 5 | `crates/core/src/langdetect/data.rs:36-37` | Так само для `l10n_rules.json` | ✅ | Побічно: `l10n_rules.json` у корені репо **не закомічений в git** (`git ls-files` не показує його), хоча потрібен для збірки — ймовірно, вже в роботі паралельного агента; не блокер портабельності готового exe |
| 6 | `crates/app/src/elevation.rs:66-96` (`relaunch_elevated`) | UAC-релонч через `ShellExecuteW("runas", exe_path, ...)`, `exe_path` з `current_exe()` | ✅ | `lpDirectory` передається як `NULL` (`:85`) → новий процес успадковує поточну робочу теку виклику, а не теку exe. Не критично (програма ніде не читає CWD), але для чистоти можна явно передати `exe.parent()` |
| 7 | `crates/app/src/app.rs:144-162` (`GameTrimmerApp::new`) | Якщо `db_path` не визначено або `db::open` падає — не панікує, показує `db_error` в UI (`ui/top_bar.rs:43-44`), працює далі з дефолтними Settings | ✅ | Текст помилки не підказує причину (тека без прав на запис) — див. розділ «Поведінка без прав на запис» |
| 8 | `crates/app/src/worker/scan.rs:56-108` (`run_scan`, завантаження правил) | Якщо `rules.json`/`l10n_rules.json` не вдалось прочитати/створити — падає назад на вбудовані дефолти (`RuleEngine::from_json(BUILTIN_RULES_JSON)`, `LangData::builtin()`), лише `Warning`, скан продовжується | ✅ | — зразковий патерн graceful degradation |
| 9 | `crates/app/src/worker/scan.rs:112-116` | Якщо `db::open(db_path)` падає під час сканування — `send_error`, скан зупиняється з читабельним повідомленням | ✅ (за дизайном) | БД — єдина тверда залежність від прав на запис; див. нижче |
| 10 | `crates/core/src/db.rs:58-63, 73-87` (`open`, `configure`) | `journal_mode=WAL`, `synchronous=NORMAL`, `cache_size`; **не задає `temp_store`** | ⚠️ | Додати `conn.pragma_update(None, "temp_store", "MEMORY")?;` — інакше `VACUUM`/сортування можуть писати тимчасові файли в `%TEMP%` (єдине реальне «протікання» за межі теки програми) |
| 11 | `crates/core/src/db.rs:75` (WAL) | WAL-режим вимагає `-wal`/`-shm` файлів поруч з БД; на мережевих дисках і деяких знімних носіях може не працювати | ⚠️ | Розглянути явну перевірку результату `PRAGMA journal_mode=WAL` (запит значення, не просто `pragma_update`) з fallback на `DELETE`, якщо драйвер повернув не `wal`. Обов'язково протестувати на реальній флешці (exFAT/FAT32) |
| 12 | `crates/app/src/worker/compact.rs` | `VACUUM` через `db::compact_observed` — успадковує ризик п. 10-11 | ⚠️ | Той самий фікс (`temp_store=MEMORY`) закриває і цей випадок |
| 13 | `crates/core/src/ops.rs:18-32` (`RecycleBin`) | `trash::delete(path)` — файли гри йдуть у Кошик Windows | ✅ (очікувана поведінка) | Це не стан програми, а стандартний механізм ОС; кошик — за замовчуванням `PermanentDelete` (`crates/core/src/settings.rs:14-20`). На знімних носіях без Кошика `trash`-крейт може повестись інакше (мовчазне permanent-delete або помилка) — варто вручну перевірити на флешці, якщо `RecycleBin` буде типовим методом для файлів поза диском C: |
| 14 | `crates/core/src/settings.rs` | `Settings`/`DeleteMethod` персистяться в таблиці `settings` тієї ж SQLite-БД поруч з exe | ✅ | — |
| 15 | `crates/app/src/worker/rules_io.rs:18-24, 39-105, 110-126` (експорт/імпорт правил) | Джерело/ціль — завжди `ensure_rules_path()`/`ensure_l10n_rules_path()` (поруч з exe); `.bak`-бекап теж поруч з exe (`backup`, `:110-126`); файл для імпорту — довільний шлях, обраний користувачем через `rfd`-діалог | ✅ | Імпорт читає довільний user-обраний файл — це очікувано (явна дія користувача), не проблема портабельності |
| 16 | Усі `crates/core/src/providers/*.rs` (`steam`, `gog`, `epic`, `ea`, `ubisoft`, `rockstar`, `battlenet`, `riot`, `amazon`, `humble`, `itch`, `xbox`) | Реєстр (`winreg::RegKey::predef` + `open_subkey`/`get_value`, ніде нема `create_subkey`/`set_value`) та `%LOCALAPPDATA%`/`%APPDATA%`/`ProgramData` — **лише читання** сторонніх лаунчерів для виявлення бібліотек ігор | ✅ | Підтверджено grep'ом: жодного запису в реєстр чи в ці теки. `amazon.rs` навіть явно відкриває SQLite `OpenFlags::SQLITE_OPEN_READ_ONLY` |
| 17 | `crates/core/src/providers/epic.rs:15`, `riot.rs:16` | Хардкод-fallback `r"C:\ProgramData\Epic\..."`, `r"C:\ProgramData"`, якщо реєстр недоступний | ✅ (read-only) | Не проблема портабельності GameTrimmer (це шлях до **чужих** даних launcher'ів, не до власного стану програми); низький ризик неточності виявлення, якщо Windows встановлено не на C: — поза межами цього аудиту |
| 18 | `crates/app/src/main.rs:17, 71-77` (`SYSTEM_FONT_PATH`) | Хардкод `r"C:\Windows\Fonts\segoeui.ttf"` для кирилиці; при помилці читання — `eprintln!` і graceful fallback на дефолтні шрифти egui (без кирилиці) | ✅ (read-only, з fallback) | Низький ризик: нетипові установки Windows (інший системний диск, урізаний Windows без цього шрифту) втратять кирилицю мовчки-графічно (не крашнеться, просто квадратики/latin-fallback). Можна в майбутньому спробувати `SHGetFontsPath`/`GetWindowsDirectoryW`, але це окрема задача, не блокер |
| 19 | `crates/core/src/mftscan/volume.rs:68-69`, `media.rs:116` | `CreateFileW` з `GENERIC_READ` (том), `FILE_SHARE_READ\|WRITE` — сирий доступ до `\\.\<letter>:` лише на читання, вимагає адміністративних прав | ✅ | Не пише нічого; вимагає елевації (див. `elevation.rs`) — очікувана поведінка Windows для сирого доступу до тому, не проблема портабельності самої програми |
| 20 | `crates/app/src/main.rs:32-35` (`NativeOptions`) | `eframe::NativeOptions::default()` без явного налаштування persistence | ✅ | Підтверджено відсутністю `directories`/`ron` у `Cargo.lock` — persistence-фіча `eframe` не активна, вікно/стан egui ніде не зберігається в `%APPDATA%`. Якщо колись оновите `eframe` і фіча стане default-on — перевірити знову |
| 21 | `crates/app/build.rs` | Вбудовує лише іконку (`winresource::WindowsResource::set_icon`) | ⚠️ | Немає Windows-маніфесту з `dpiAwareness`. Додати `set_manifest`/`set_manifest_file` — див. розділ «Пакування» |
| 22 | Кореневий `Cargo.toml`, `[profile.release]` | `panic = "abort"`, `strip = true`, `lto = "fat"`, `codegen-units = 1`, `opt-level = "z"` | ✅ для портабельності / ⚠️ побічно | `strip = true` — добре (менший, чистіший exe без шляхів налагодження). `panic = "abort"` конфліктує з `catch_unwind`-очікуваннями в `scan.rs:528`, `mftscan/mod.rs:151` — не проблема портабельності, але варто окремо переглянути (див. «Побічні спостереження») |
| 23 | Немає — перевірено окремо | `std::env::set_current_dir` / `std::env::current_dir()` | ✅ | Не використовується ніде в `crates/` — програма повністю ігнорує CWD |
| 24 | Немає — перевірено окремо | `SHGetKnownFolderPath`/`SHGetFolderPath`/`FOLDERID_*`/`dirs::home_dir` | ✅ | Не використовується — немає прихованих залежностей від «домашньої теки» |
| 25 | Немає — перевірено окремо | Краш-дампи/minidump-бібліотеки | ✅ | Не використовується жодна бібліотека дампів; будь-який дамп при `abort()` — це стандартний Windows Error Reporting (`%LOCALAPPDATA%\CrashDumps`, якщо в системі увімкнено), не контролюється й не створюється програмою напряму — це поведінка ОС, не «слід» програми |

## Поведінка в теці без прав на запис (Program Files, read-only диск)

Перевірено логічно через код (не запускалось руками — див. чек-лист нижче для ручної перевірки):

1. **`gametrimmer.db` не вдається створити** (`crates/core/src/db.rs:59`, `Connection::open` поверне помилку прав доступу) →
   `crates/app/src/app.rs:153-156` ловить це, показує `db_error = "Помилка відкриття бази даних: {err}"` у верхній панелі (`crates/app/src/ui/top_bar.rs:43-44`), програма **не падає**, стартує з порожнім станом (`Settings::default()`).
2. Кнопка «Сканувати бібліотеки» (`start_scan`, `app.rs:412-435`) все одно активна (бо `db_path` — це просто `Some(PathBuf)`, обчислений з `current_exe()`, а не перевірка прав) → скан стартує, і вже в `worker/scan.rs:112-116` `db::open` падає вдруге, надсилає `WorkerMsg::Error` з тим самим текстом помилки. **Сканування фактично неможливе без прав на запис у теку програми** — це архітектурне обмеження (SQLite-БД — обов'язкова, не є "приємним доповненням"), і виправити його зараз не варто (означало б in-memory-режим без збереження результатів — окрема, велика задача поза межами цього аудиту).
3. `rules.json`/`l10n_rules.json` у тій самій ситуації теж не вдасться створити — але це вже не критично, бо `run_scan` (п. 8 таблиці) падає назад на вбудовані правила з попередженням, а не помилкою.
4. **Рекомендований UX-фікс (P1, дрібний)**: доповнити текст помилки в `app.rs:154` і `scan.rs:115` явною підказкою, напр.:
   `"Помилка відкриття бази даних: {err}. Перемістіть програму в теку з правами на запис (не Program Files без прав адміністратора)."`
   Це не архітектурна зміна — просто рядок тексту, безпечно для паралельного агента (файли не перетинаються з langdetect/packs/rules_io/README).

Висновок: **немає жодного сценарію, де програма щось пошкодить, залишить сирітські файли деінде або крашнеться** через відсутність прав на запис — вона або працює, або показує зрозумілу (хоч і без конкретної підказки поки що) помилку.

## Пакування

### Вміст портабельного zip

```
GameTrimmer-<version>-portable-win64.zip
├── gametrimmer.exe        # release, strip=true (вже в Cargo.toml)
├── rules.json             # копія кореневого rules.json — щоб перший запуск
│                          # не потребував права на запис для матеріалізації
├── l10n_rules.json        # те саме для мовних правил
├── README.md
└── LICENSE
```

Чому варто класти `rules.json`/`l10n_rules.json` в zip одразу, а не покладатись лише на `ensure_data_file_in` (яка й так їх створить при першому запуску): це прибирає один із двох випадків запису при першому старті. Залишається тільки `gametrimmer.db`, яка є обов'язковою і без обхідних шляхів (див. вище) — тобто портабельний пакет одразу дає користувачу зрозуміти: «якщо ця тека доступна для запису — все працює».

### Скрипт збирання (PowerShell, запропонований `scripts/package-portable.ps1`)

> **Стан на 2026-08-03.** Скрипт створено — він живе в `scripts/package-portable.ps1`
> і саме він є актуальним джерелом. Блок нижче лишено як запис пропозиції; від
> реального файлу він уже відрізняється (пакує обидва README і
> `THIRD-PARTY-NOTICES.md`, чистить вміст стейджинг-теки замість видалення самої
> теки). **Причина, названа нижче, застаріла:** `.cargo/config.toml` прибрано
> 2026-07-24 (`6814264`), і `target-dir` — звичайний `target/` у корені проєкту.
> Через `cargo metadata` шлях обчислюється й досі, але з іншої причини: target-dir
> може зсунути `CARGO_TARGET_DIR` чи `build.target-dir`, а `cargo metadata`
> відповідає з того самого джерела, з якого збирає збірка.

Наступний агент може створити цей файл буквально за цим текстом — він враховує зовнішній `target-dir` з `.cargo/config.toml` (`E:/Mancubus/Projects/GameTrimmer-target` — через кириличний шлях проєкту, див. `.cargo/config.toml`), тому шлях до exe **не** `target/release/...`, а обчислюється через `cargo metadata`:

```powershell
# scripts/package-portable.ps1
# Збирає release-версію GameTrimmer і пакує портабельний zip.
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

    Write-Host "Готово: $zipPath"
}
finally {
    Pop-Location
}
```

Застереження: перед реалізацією перевірте, що `l10n_rules.json` дійсно існує в корені репо й актуальний (наразі не в git — див. знахідку №5); якщо паралельний агент його переносить/перейменовує, скрипт треба звірити з фінальним станом `crates/core/src/langdetect/data.rs`.

### DPI-масштабування — чек-лист ручної перевірки

Код-рівень: `eframe` 0.35.0 → `winit` 0.30.13 (`Cargo.lock:4492-4494`). Сучасні версії `winit` для Windows самі викликають `SetProcessDpiAwarenessContext(...PER_MONITOR_AWARE_V2)` під час ініціалізації віконної системи, **без** явного маніфесту. Маніфесту в проєкті зараз немає (`crates/app/build.rs` вбудовує лише іконку). Тобто програма, ймовірно, вже DPI-aware «з коробки», але це твердження **не перевірено запуском** — обов'язково перевірити руками:

1. Запустити на моніторі зі 100% масштабуванням — інтерфейс різкий, розміри очікувані.
2. Перемкнути Windows на 150% (Параметри → Система → Дисплей), перезапустити програму (або перетягнути вікно на монітор з іншим масштабом, якщо є кілька моніторів) — текст/іконки мають масштабуватись чітко, без розмиття (розмиття = ознака DPI-unaware/bitmap-stretch).
3. Те саме на 200%.
4. Якщо є фізично два монітори з різним масштабуванням (напр. 100% і 150%) — перетягнути вікно між ними «наживо» й перевірити, що масштаб підхоплюється без релогіну/розмиття (це і є per-monitor v2, на відміну від просто system-aware).
5. Якщо десь у пп. 2-4 з'явиться розмиття — це ознака, що `winit`/`eframe` 0.35 з якоїсь причини не проставили DPI-awareness контекст (можливо, конфлікт з іншим маніфестом, або регресія версії) → тоді обов'язково додати явний маніфест (нижче).

### Явний маніфест (рекомендовано як defense-in-depth, навіть якщо п. 4-5 вище пройшли без розмиття)

Додати в `crates/app/build.rs` (поруч з `set_icon`, той самий `winresource::WindowsResource`):

```rust
resource.set_manifest_file("assets/gametrimmer.manifest");
```

(Перевірте на docs.rs, що в `winresource = "0.1.23"` метод називається саме так — API дзеркалить старіший `winres`, де це `set_manifest_file`/`set_manifest`.)

Новий файл `crates/app/assets/gametrimmer.manifest`:

```xml
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="GameTrimmer.GameTrimmer" version="1.0.0.0" processorArchitecture="*"/>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 і 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
      <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
    </application>
  </compatibility>
</assembly>
```

`longPathAware` тут доречний окремо від DPI: якщо флешка/тека монтування дає довгий шлях (напр. `E:\Ігри\Портативні програми\GameTrimmer\...`), це знімає обмеження MAX_PATH=260 для операцій самої програми (`dunce::canonicalize` у `scan.rs:485` вже частково рятує від `\\?\`-префіксів, але системний маніфест — надійніше на рівні ОС).

## Пріоритезований план імплементації

> **Статус: реалізовано 2026-07-19** (крім ручних перевірок P2, які лишаються на користувача). Деталі й відхилення — під кожним пунктом.

**P0 — вже добре, лише зафіксувати регресійним тестом/чек-листом (немає змін коду):**
1. ✅ Перевірити (юніт-тест або ручний прогін), що `db_path()`, `ensure_rules_path()`, `ensure_l10n_rules_path()` завжди повертають шлях відносно `current_exe()`, незалежно від CWD запуску (напр. запустити exe подвійним кліком із Провідника **і** з `cmd.exe` в іншій теці — результат має бути ідентичний). **Зроблено юніт-тестом** `worker::tests::db_path_is_independent_of_current_working_directory` (`crates/app/src/worker/mod.rs`) — змінює CWD процесу й перевіряє незмінність `db_path()`, безпечно завдяки підтвердженому фактом, що ніде в кодовій базі CWD не читається (знахідка №23). Ручний подвійний прогін (Провідник vs `cmd.exe`) не виконувався — юніт-тест логічно покриває той самий інваріант.
2. ✅ Зафіксувати в тестах/CI, що `Cargo.lock` не містить `directories`/`ron` — **зроблено** тестом `portability_regression_tests::cargo_lock_does_not_pull_in_eframe_persistence_deps` (`crates/core/src/lib.rs`), grep по вмісту `Cargo.lock`.

**P1 — дрібні, безпечні, безконфліктні з паралельною роботою:**
3. ✅ `crates/core/src/db.rs`, `configure()`: додано `conn.pragma_update(None, "temp_store", "MEMORY")?;` + тест `db::tests::open_sets_temp_store_to_memory`.
4. ✅ `crates/app/src/app.rs:154` і `crates/app/src/worker/scan.rs:115`: текст помилки відкриття БД доповнено підказкою про права на запис — рядок один-в-один з формулюванням із розділу «Поведінка без прав на запис» вище.
5. ✅ `crates/app/build.rs` + новий `crates/app/assets/gametrimmer.manifest`: DPI/longPath-маніфест додано (`resource.set_manifest_file(...)`, `winresource` 0.1.31 — метод підтверджено читанням вихідного коду крейта, не документації). Робилося **до**, а не після ручної DPI-перевірки (розбіжність із формулюванням цього пункту): агент без доступу до фізичних моніторів різної щільності не може виконати ручний чек-лист, тож defense-in-depth додано одразу, а ручна перевірка 100/150/200% лишається як відкритий хвіст користувачу (чек-лист нижче не змінено). Перевірено грепом по скомпільованому exe, що рядки `dpiAware`, `PerMonitorV2`, `longPathAware` реально вбудувалися в ресурс.
6. ✅ Створено `scripts/package-portable.ps1` за наведеним текстом, з одним доповненням: наприкінці виводить розмір exe і zip у МБ (не було в оригінальному тексті скрипта, додано для зручності перевірки після кожного пакування). Прогнано наживо: `dist/GameTrimmer-0.1.0-portable-win64.zip` (5.58 МБ) з exe 12.58 МБ + `rules.json` + `l10n_rules.json` + `README.md` + `LICENSE` — розпаковано й звірено вміст.

**Понад оригінальний P1-список** (не було окремим пунктом плану, але вимагалося завданням і відповідає знахідці №11 таблиці вище):
   ✅ WAL-фолбек — `crates/core/src/db.rs`, нова `configure_journal_mode()`: читає результат `PRAGMA journal_mode = WAL` замість ігнорування (як робив `pragma_update`) і явно переходить на `DELETE`, якщо WAL не застосувався. Тест `db::tests::configure_does_not_fail_when_wal_is_unavailable` використовує `:memory:`-з'єднання (SQLite там теж не підтримує WAL) як відтворюваний без реального мережевого диска/флешки шлях до гілки фолбеку.

**P2 — потребують ручного тестування на реальному носії, робити перед релізом:**
7. ⬜ Запустити зібраний портабельний zip з реальної USB-флешки (по можливості і exFAT, і FAT32) — перевірити: створення БД, WAL-файлів, сканування, видалення (обома методами — Permanent і RecycleBin). **Не виконано агентом** — потребує фізичного носія, лишається користувачу.
8. ⬜ Розпакувати в `C:\Program Files\GameTrimmer` без прав адміністратора — перевірити, що з'являється зрозуміла помилка (п. 4 вище), а не тиха відмова чи краш. **Не виконано** — потребує ручного UAC-сценарію.
9. ⬜ Прогнати DPI-чек-лист (100/150/200%, per-monitor якщо є обладнання). **Не виконано** — див. чек-лист нижче.
10. ⬜ (Опційно) Перевірити поведінку `trash::delete` для файлів на знімному носії без Кошика. **Не виконано**, опційно.

**P3 — за первісним планом поза межами аудиту, але виконано за прямою вказівкою в завданні імплементації:**
11. ✅ `panic = "abort"` → `panic = "unwind"` у кореневому `[profile.release]` (`Cargo.toml`). **Це свідоме відхилення від власної пріоритезації аудиту** (котрий відклав це в P3, «не робити зараз») — зроблено, бо завдання на імплементацію прямо вимагало розв'язати конфлікт `panic=abort` × `catch_unwind` у `scan.rs:528`/`mftscan/mod.rs:151`. Виміряна ціна на цьому дереві (opt-level=z, lto=fat, codegen-units=1, strip=true в обох випадках): **10 913 792 байт з "abort" проти 13 189 120 байт з "unwind" — +2.18 МіБ (+20.9%)**, суттєво більше за початкову оцінку «кілька сотень КіБ» у знахідці №22 вище. Рішення лишили на користь "unwind": втрата ізоляції паніки провайдера/тому означає, що будь-яка паніка вбиває весь процес і багатогодинний скан без збереження прогресу — гірший наслідок для користувача, ніж +2.18 МіБ у портативному zip. Якщо розмір стане критичним, альтернатива — прибрати сам механізм `catch_unwind` (і прийняти crash-on-panic), а не тримати його мертвим кодом під "abort".

## Побічні спостереження (поза основним обсягом задачі)

- `l10n_rules.json` у корені репозиторію потрібен на етапі компіляції (`include_str!` у `crates/core/src/langdetect/data.rs:37`), але не закомічений у git (`git ls-files` його не показує, на відміну від `rules.json`, який закомічений). Ймовірно, в процесі роботи паралельного агента над `langdetect/`/`packs.rs` — варто звірити перед тим, як покладатись на нього в скрипті пакування.
  - _Закрито: обидва файли закомічені._
- `.cargo/config.toml` виносить `target-dir` за межі проєкту (`E:/Mancubus/Projects/GameTrimmer-target`) через відомий баг `autocfg` з кириличними шляхами — це стосується **збірки**, не рантайм-портабельності готового exe, але важливо для скрипту пакування (враховано вище через `cargo metadata`).
  - _Неактуально з 2026-07-24 (`6814264`): обхід прибрано, `.cargo/` порожня, збірка кладе все у звичайний `target/` у корені. Кириличний шлях `E:\Mancubus\Projects\Vibecoding\GameTrimmer` більше не заважає — див. шапку розділу «Скрипт збирання» вище._
