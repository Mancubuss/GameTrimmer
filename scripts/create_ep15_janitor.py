import json
import subprocess
import urllib.request
import urllib.error
import sys

sys.stdout.reconfigure(encoding='utf-8')

# 1. Get decrypted token from DPAPI via powershell
ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\antigravity-agent.api-token.dpapi")
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""

res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True)
token = res.stdout.strip()
if not token:
    raise RuntimeError("Failed to obtain decrypted Vikunja API token.")

headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

project_id = 5

epic_desc = """# Епік GT-EP15: Розширення каталогу очищення: Воркшоп, незавершені завантаження, шейдери, дампи та ігрові кеші (Next-Gen Game & Engine Artifact Janitor)

- **Батьківський проект:** GameTrimmer (v1.1.0+)
- **Статус епіку:** Затверджено до реалізації / До виконання
- **Загальний потенціал економії на реальній системі користувача:** **35 – 70+ ГБ** (підтверджено емпіричним аудитом системи розробника: 8.56 ГБ шейдерів, 18.95 ГБ воркшопу, >4.5 ГБ застарілих автосейвів, 570 МБ кешів лаунчерів та понад 250 ГБ катсцен).

---

### Головна мета та філософія епіку:
Розширити діапазон сканування GameTrimmer на найбільш докучливі та важкі артефакти геймінгу, які традиційні чистильники ігнорують або видаляють небезпечно:
1. **Steam Downloading / Staging:** Очищення покинутих гігабайтних чанків завантажень без скидання авторизації 2FA Steam.
2. **Steam Workshop Orphan Cleaner:** Виявлення забутих модів відписаних або деінстальованих ігор на основі парсингу `appworkshop_*.acf`.
3. **Smart Shader Cache Cleaner:** Зачистка застарілих кешів драйверів GPU (NVIDIA DXCache / AMD DxCache) та видалених AppID без виклику мікрофризів у поточно активних іграх.
4. **Crash Dumps & Runaway Logs:** Безпечне видалення зліпків пам'яті `.dmp` Unreal Engine (`Saved/Crashes`), Unity (`Player.log`) та Windows WER.
5. **Save Bloat & Autosave Pruner:** Розумне проріджування квіксейвів (Smart Retention) для важких RPG (Skyrim 998 сейвів, BG3, Starfield, OpenMW, Cyberpunk) із захисним бекапом.
6. **Launcher Web/CEF Caches & Mod Managers:** Очищення Chromium-кешів лаунчерів та завантажувальних архівів Vortex/MO2.

---

### Склад спайків та завдань епіку:
1. **GT-181:** [Спайк: Воркшоп та незавершені завантаження Steam/EGS] Дослідження та парсинг маніфестів `appworkshop_*.acf`, безпечна детекція осиротілих модів та незавершених депотів у `steamapps/downloading` та `.egstore/Pending` без скидання 2FA-сесій лаунчерів.
2. **GT-182:** [Спайк: Шейдерні кеші GPU та Steam] Аналіз форматів кешу NVIDIA (`DXCache`/`GLCache`), AMD (`DxCache`), Windows `D3DSCache` та `steamapps/shadercache`: селективна зачистка застарілих версій драйверів та видалених AppID без шкоди для активних ігор.
3. **GT-183:** [Спайк: Дампи падінь та логи ігрових рушіїв] Картографування смітників Unreal Engine (`Saved/Crashes`, `Saved/Logs`), Unity (`Player.log`), Windows WER (`CrashDumps`) та розробницьких діагностичних артефактів без втручання в живі процеси.
4. **GT-184:** [Спайк: Проріджування та ротація автосейвів] Дослідження структури сейвів та квіксейвів у важких RPG (BG3, Starfield, Cyberpunk, Paradox), реалізація алгоритму безпечного проріджування (Smart Retention N-last) та автоматичного створення захисних zip-архівів.
5. **GT-185:** [Спайк: Мод-менеджери та веб-кеші лаунчерів] Дослідження схем збереження завантажень у Vortex, Mod Organizer 2, CurseForge та очищення CEF/Chromium кешів лаунчерів (Steam `htmlcache`, Epic, EA, Ubisoft).
6. **GT-186:** [Імплементація: Core Engine & Manifest Rules] Розширення `crates/core` новими категоріями та правилами: парсери воркшопу, staging-детектори, розширення `rules.json` для UE/Unity/WER дампів та інтеграція в єдиний сканер.
7. **GT-187:** [Імплементація: UI/UX нових категорій та Save/Workshop менеджер] Додавання нових вкладок або категорій у дерево знахідок `crates/app`, селектори безпеки (Safety Badges ⚠) та окремий режим перегляду для Воркшопу та автосейвів.
"""

tasks = [
    {
        "title": "GT-EP15 · Розширення каталогу очищення: Воркшоп, незавершені завантаження, шейдери, дампи та ігрові кеші (Next-Gen Game & Engine Artifact Janitor)",
        "priority": 4,
        "description": epic_desc
    },
    {
        "title": "GT-181 · [Спайк: Воркшоп та незавершені завантаження Steam/EGS] Дослідження та парсинг маніфестів appworkshop_*.acf, безпечна детекція осиротілих модів та незавершених депотів у steamapps/downloading та .egstore/Pending без скидання 2FA-сесій",
        "priority": 3,
        "description": """# Спайк GT-181: Воркшоп та незавершені завантаження Steam / Epic Games

- **Мета:** Дослідити алгоритми детекції та безпечного видалення покинутих депотів завантаження та осиротілих модів Steam Workshop.
- **Емпіричні дані з системи розробника:**
  - `steamapps/workshop/content`: **18.95 GB** на дисках F: та G: (AppID 362890, 217140, 564310).
  - `steamapps/downloading`: **44.65 MB** (покинуті залишки в H:\\SteamLibrary).
- **Ключові технічні задачі:**
  1. **Парсинг маніфестів Steam Workshop:** Читання `steamapps/workshop/appworkshop_<appid>.acf` за допомогою KeyValues парсера, вилучення списку підписок `WorkshopItemDetails`.
  2. **Детекція осиротілих модів:** Порівняння папок у `steamapps/workshop/content/<appid>/<mod_id>` зі списком активних підписок. Якщо папки немає у списку або AppID гри відсутній у бібліотеці — класифікація як `orphan_workshop_item`.
  3. **Безпечне очищення `steamapps/downloading` та `temp`:** Перевірка `appmanifest_<appid>.acf` (`StateFlags != 1026`, не в черзі завантаження) та відсутність `ERROR_SHARING_VIOLATION` (Steam не тримає активний дескриптор).
  4. **Перевага над штатним засобом Steam:** Steam вимагає "Clear Download Cache", що призводить до повного розлогінювання та скидання 2FA сесії. GameTrimmer робить точкове очищення без втрати авторизації.
"""
    },
    {
        "title": "GT-182 · [Спайк: Шейдерні кеші GPU та Steam] Аналіз форматів кешу NVIDIA (DXCache/GLCache), AMD (DxCache), Windows D3DSCache та steamapps/shadercache: селективна зачистка застарілих версій драйверів та видалених AppID",
        "priority": 3,
        "description": """# Спайк GT-182: Шейдерні кеші GPU (NVIDIA / AMD) та Steam Pipeline Cache

- **Мета:** Розробити алгоритм селективної зачистки кешу шейдерів без виклику мікрофризів у поточно встановлених іграх.
- **Емпіричні дані з системи розробника:**
  - `NVIDIA DXCache`: **8.55 GB** (153 файли у `%LOCALAPPDATA%\\NVIDIA\\DXCache`).
  - `NVIDIA GLCache` & `D3DSCache`: ~7.5 MB.
  - `Steam Shadercache`: **575.62 MB** (виявлено осиротілі кеші деінстальованих ігор).
- **Ключові технічні задачі:**
  1. **Аналіз структури `DXCache` (NVIDIA):** Визначення версії драйвера в заголовках `.bin`/`.toc`. При оновленні відеодрайвера старий кеш стає "мертвим" для читання новим драйвером, але не видаляється.
  2. **Очищення `steamapps/shadercache/<appid>`:** Порівняння списку AppID з активними бібліотеками; видалення Fossilize / DXVK / Vulkan пайплайнів для ігор, які вже деінстальовані.
  3. **Диференціація від стандартного Windows Disk Cleanup:** Стандартний інструмент Windows бездумно стирає весь `D3DSCache`, змушуючи всі ігри заново компілювати шейдери при наступному запуску. GameTrimmer застосовує фільтрацію за давністю доступу (mtime > 30-60 днів) та зв'язком з AppID.
"""
    },
    {
        "title": "GT-183 · [Спайк: Дампи падінь та логи ігрових рушіїв] Картографування смітників Unreal Engine (Saved/Crashes, Saved/Logs), Unity (Player.log), Windows WER (CrashDumps) та розробницьких діагностичних артефактів",
        "priority": 3,
        "description": """# Спайк GT-183: Дампи падінь та діагностичні логи ігрових рушіїв

- **Мета:** Створити правила та механізми безпечної зачистки зліпків пам'яті падінь та роздутих лог-файлів ігор.
- **Емпіричні дані з системи розробника:**
  - `Windows WER CrashDumps`: **30.47 MB** у `%LOCALAPPDATA%\\CrashDumps`.
  - `Unreal Engine Saved/Crashes & Logs`: виявлено логи та дампи для `Alabama`, `DeadIsland`, `EpicGamesLauncher`, `project_A`, `Xipetotec` тощо.
- **Ключові технічні задачі:**
  1. **Unreal Engine 4 & 5 Crash Structure:** Сканування папок `%LOCALAPPDATA%/<GameName>/Saved/Crashes/` та `<GameRoot>/Saved/Crashes/` (кожен збій генерує Minidump `.dmp`, `CrashContext.runtime-xml`, `XMLReport`).
  2. **Unity Player Logs:** Детекція `%USERPROFILE%/AppData/LocalLow/<Company>/<Game>/Player.log` та `Player-prev.log` (захист від випадків розростання до 10-50 ГБ при зацикленні аудіо/рендерера).
  3. **Windows Error Reporting (WER):** Очищення `%LOCALAPPDATA%/CrashDumps/*.dmp`, що належать виконуваним файлам ігор.
  4. **Рівень безпеки:** 100% безпечно для закритих ігор. Жоден рушій не використовує старі дампи для роботи гри.
"""
    },
    {
        "title": "GT-184 · [Спайк: Проріджування та ротація автосейвів] Дослідження структури сейвів та квіксейвів у важких RPG (BG3, Starfield, Cyberpunk, Paradox), реалізація алгоритму безпечного проріджування (Smart Retention) та захисних zip-архівів",
        "priority": 3,
        "description": """# Спайк GT-184: Проріджування автосейвів та збережень (Smart Save Retention)

- **Мета:** Вирішити проблему розростання папок збережень у важких RPG та зависання синхронізації Steam Cloud.
- **Емпіричні дані з системи розробника:**
  - `Skyrim Special Edition`: **1.79 GB** (998 файлів збережень у `Documents\\My Games\\Skyrim Special Edition`).
  - `OpenMW`: **1.62 GB** (25 файлів у `Documents\\My Games\\OpenMW`).
  - `CD Projekt Red (Cyberpunk / Witcher)`: **380.75 MB** (195 файлів).
  - `Metro Exodus`: **138.27 MB** (35 файлів).
  - `Kingdom Come Deliverance 2`: **44.64 MB** (571 файл у `Saved Games`).
  - `MachineGames & id Software`: **108.5 MB** (понад 1,000 файлів).
  - **Сумарний обсяг на машині користувача:** **> 4.5 GB** у збереженнях.
- **Ключові технічні задачі:**
  1. **Класифікація збережень:** Відокремлення ручних іменованих збережень (`ManualSave_*`) від автоматичних (`AutoSave_*`, `QuickSave_*`).
  2. **Алгоритм Smart Retention (N-Last):** Збереження останніх $N$ квіксейвів (наприклад, 5 найновіших на персонажа) та пропонування до видалення/архівації застарілих проміжних слотів (наприклад, від QuickSave 1 до QuickSave 990).
  3. **Zero-Data-Loss Safety Shield:** Обов'язкове створення єдиного бекап-архіву (`GameTrimmer_Backup_Skyrim_*.zip`) у локальну теку бекапів перед будь-яким видаленням сейвів.
  4. **Полегшення Steam Cloud Sync:** Усунення багатовізуальних тайм-аутів завантаження сейвів у хмару Steam при закритті ігор.
"""
    },
    {
        "title": "GT-185 · [Спайк: Мод-менеджери та веб-кеші лаунчерів] Дослідження схем збереження завантажень у Vortex, Mod Organizer 2, CurseForge та очищення CEF/Chromium кешів лаунчерів (Steam htmlcache, Epic, EA, Ubisoft)",
        "priority": 3,
        "description": """# Спайк GT-185: Зачистка завантажень мод-менеджерів та вбудованих веб-кешів лаунчерів

- **Мета:** Дослідити можливість очищення архівних копій завантажень модів та веб-кешів ігрових клієнтів.
- **Емпіричні дані з системи розробника:**
  - `Steam HTML Cache`: **418.48 MB** (4,287 файлів у `%LOCALAPPDATA%\\Steam\\htmlcache`).
  - `Ubisoft Connect Cache`: **151.72 MB** (1,556 файлів).
  - `GOG Galaxy Web Cache`: **8.44 MB**.
  - Загальний обсяг веб-кешів на машині: **~570 MB** (5,843 файли).
- **Ключові технічні задачі:**
  1. **CEF / Chromium Web Caches:** Дослідження структури кешу браузерів лаунчерів (`Steam/htmlcache`, `EpicGamesLauncher/Saved/webcache`, `Ubisoft Connect/cache`, `EA Desktop/CEF`). Всі ці папки містять тимчасові банери, відео з магазину та JS-кеш, які автоматично регенеруються за потреби.
  2. **Mod Managers Staging Archives:** Детекція папок завантажень у Vortex (`AppData/Roaming/Vortex/downloads`), MO2 (`downloads/`), CurseForge (`AppData/Roaming/CurseForge`). Пошук архівних zip/7z пакетів, моди з яких уже розпаковані та встановлені.
"""
    },
    {
        "title": "GT-186 · [Імплементація: Core Engine & Manifest Rules] Розширення crates/core новими категоріями та правилами: парсери воркшопу, staging-детектори, розширення rules.json для UE/Unity/WER дампів та інтеграція в єдиний сканер",
        "priority": 4,
        "description": """# Завдання GT-186: Імплементація нових категорій та парсерів у crates/core

- **Мета:** Інтегрувати нові категорії та алгоритми очищення в ядро `crates/core`.
- **Компоненти розробки:**
  1. **Категорії правил у `crates/core/src/rules.rs` та `rules.json`:**
     - `Category::WorkshopOrphan` — осиротілі моди воркшопу.
     - `Category::DownloadStaging` — незавершені завантаження Steam/Epic.
     - `Category::ShaderCache` — застарілі шейдерні кеші.
     - `Category::CrashDump` — дампи та системні логи падінь.
     - `Category::LauncherWebCache` — Chromium/CEF кеші лаунчерів.
  2. **Модуль `crates/core/src/workshop.rs`:**
     - KeyValues парсер для `appworkshop_<appid>.acf`.
     - Зіставлення каталогу `workshop/content/<appid>` зі списком підписок.
  3. **Модуль `crates/core/src/staging.rs`:**
     - Сканування `steamapps/downloading` та `.egstore/Pending`.
     - Перевірка статусів завантаження та відкритих файлових дескрипторів.
  4. **Модуль `crates/core/src/saves.rs` (Опціональний модуль безпеки):**
     - Детектори структури сейвів (Bethesda `.ess`, Larian `.lsv`, CDPR `.dat`/`.sav`).
     - Створення zip-бекапів перед операціями.
  5. **100% тест-покриття:** Юніт-тести для всіх нових категорій, збереження 100% Rust memory safety та zero-panic гарантій.
"""
    },
    {
        "title": "GT-187 · [Імплементація: UI/UX нових категорій та Save/Workshop менеджер] Додавання нових вкладок або категорій у дерево знахідок crates/app, селектори безпеки (Safety Badges ⚠) та окремий режим перегляду",
        "priority": 4,
        "description": """# Завдання GT-187: Інтеграція нових категорій у інтерфейс crates/app (egui)

- **Мета:** Надати користувачеві інтуїтивний, прозорий та безпечний інтерфейс для перегляду та керування новими знахідками.
- **Компоненти розробки:**
  1. **Оновлення дерева знахідок:**
     - Додавання нових груп: *«Незавершені завантаження»*, *«Осиротілий Воркшоп»*, *«Шейдерний кеш GPU»*, *«Дампи падінь та логи»*, *«Кеш лаунчерів»*.
  2. **Safety Badges (⚠) та тултіпи впевненості:**
     - Чітке маркування безпеки видалення (наприклад, Дампи падінь = 100% Зелений рівень безпеки; Воркшоп = перевірено проти маніфесту; Сейви = створення бекапу).
  3. **Спеціалізоване вікно «Менеджер автосейвів (Smart Save Pruner)»:**
     - Відображення ігор з великою кількістю сейвів (наприклад, *Skyrim: 998 сейвів, 1.79 GB*).
     - Повзунок збереження останніх $N$ сейвів (дефолт: 5).
     - Велика кнопка «Створити ZIP-бекап та прорідити».
  4. **Експорт у CSV / Журнал дій:** Оновлення структури звітів про звільнене місце.
"""
    }
]

created_ids = []

for idx, t in enumerate(tasks):
    payload = {
        "title": t["title"],
        "description": t["description"],
        "priority": t["priority"]
    }
    data = json.dumps(payload, ensure_ascii=False).encode('utf-8')
    req = urllib.request.Request(f"http://127.0.0.1:3456/api/v2/projects/{project_id}/tasks", data=data, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req) as resp:
            res = json.loads(resp.read().decode('utf-8'))
            tid = res.get('id')
            tidx = res.get('index')
            created_ids.append((tid, tidx, t["title"]))
            print(f"Created task [{idx+1}/{len(tasks)}]: ID {tid}, Index {tidx} -> {t['title'][:60]}")
    except urllib.error.HTTPError as e:
        err = e.read().decode('utf-8')
        print(f"Error creating task {idx+1}: HTTP {e.code} - {err}")
        sys.exit(1)

# Link subtasks to epic
epic_id = created_ids[0][0]
child_ids = [c[0] for c in created_ids[1:]]

print(f"\nLinking {len(child_ids)} child tasks to Epic ID {epic_id}...")

for cid in child_ids:
    payload = {
        "other_task_id": cid,
        "relation_kind": "subtask"
    }
    data = json.dumps(payload).encode('utf-8')
    req = urllib.request.Request(f"http://127.0.0.1:3456/api/v1/tasks/{epic_id}/relations", data=data, headers=headers, method="PUT")
    try:
        with urllib.request.urlopen(req) as resp:
            res = json.loads(resp.read().decode('utf-8'))
            print(f"Linked child {cid} to epic {epic_id}")
    except urllib.error.HTTPError as e:
        err = e.read().decode('utf-8')
        print(f"Error linking {cid}: HTTP {e.code} - {err}")

print("\n--- ALL TASKS AND RELATIONS CREATED SUCCESSFULLY IN VIKUNJA ---")
