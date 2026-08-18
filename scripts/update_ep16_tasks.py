import json
import subprocess
import urllib.request
import urllib.error
import sys

sys.stdout.reconfigure(encoding='utf-8')

# 1. Get decrypted token from DPAPI via powershell
ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\google-antigravity-agent.api-token.dpapi")
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

# Update tasks with simplified Ponytail descriptions
task_updates = {
    360: {
        "title": "GT-EP16 · Модульна зовнішня система локалізації UI (Community-Driven External Localization & i18n Engine)",
        "description": """# Епік GT-EP16: Модульна зовнішня система локалізації UI (Community-Driven External Localization & i18n Engine)

- **Батьківський проект:** GameTrimmer (v1.1.0+)
- **Статус епіку:** В роботі / Реалізація (Ponytail Lean Architecture)
- **Головна мета:** Повне винесення всіх інтерфейсних рядків та динамічних повідомлень за межі скомпільованого бінарного exe-файлу у відкриті файли локалізації (JSON), що дозволить спільноті виправляти помилки, адаптувати термінологію та самостійно додавати нові мови без перезбирання програми з вихідного коду.

---

### Спрощена архітектурна концепція (Ponytail-підхід):
1. **Типізована десеріалізація через `serde_json`:** `struct Strings` отримує `#[derive(Deserialize, Clone)]`. Файли `locales/en.json` та `locales/uk.json` десеріалізуються безпосередньо в структуру в 1 рядок коду.
2. **Нуль змін у викликах UI:** Жодного переписування 200+ викликів `s.btn_scan_libraries` на динамічні lookup'и в HashMap. Типізація та швидкість компілятора зберігаються на 100%.
3. **Zero-Broken-UI Shield (Каскадний Fallback):** Вшиті `include_str!("locales/en.json")` та `include_str!("locales/uk.json")`. Якщо зовнішній файл відсутній або в ньому не вистачає ключа, значення підтягується з англійського оригіналу.
4. **Легка плюралізація:** Компактна функція для германських (1 / other) та слов'янських (1 / 2-4 / 5+) форм без сторонніх важких бібліотек.
5. **Зручний UI:** ComboBox у `Settings -> General` зі списком знайдених файлів у `locales/` + кнопка «Відкрити теку локалізацій».
"""
    },
    361: {
        "title": "GT-188 · [Спайк: Архітектура та вибір формату зовнішньої локалізації] Structured JSON без додаткових залежностей",
        "description": "Спайк успішно завершено. Обрано Structured JSON, десеріалізований через `serde_json` напряму в `struct Strings`. 0 нових залежностей, максимальна простота для авторів перекладів."
    },
    362: {
        "title": "GT-189 · [Спайк: Модель відмовостійкості, каскадний Fallback та Zero-Panic деградація] Серде-фолбек на вбудований en.json",
        "description": "Спайк успішно завершено. Прийнято 3-рівневий каскад: Custom JSON -> Embedded Native -> Embedded English Baseline. Логи завжди пишуться англійською (Reported)."
    },
    363: {
        "title": "GT-190 · [Ядро: Динамічне сканування та конфігурація мов] Сканування locales/ та оновлення налаштувань",
        "description": "Сканування директорії `locales/*.json` поруч з exe та в `%LOCALAPPDATA%/GameTrimmer/locales/`. Збереження вибору мови у `gametrimmer.ini` (`app_language = <tag>`)."
    },
    364: {
        "title": "GT-191 · [Ядро: Рушій завантаження перекладів та базова плюралізація] Серде-десеріалізатор Strings та лаконічний plural helper",
        "description": "Завантаження JSON у `struct Strings` з підстановкою значень за замовчуванням. Проста 15-рядкова функція відмінювання числівників (1, 2-4, 5+)."
    },
    365: {
        "title": "GT-192 · [UI/UX: Динамічний селектор мов та відкриття теки] ComboBox у Settings -> General",
        "description": "Заміна радіокнопок у Settings -> General на ComboBox з переліком доступних мов + кнопка виклику провідника до папки `locales/`."
    },
    366: {
        "title": "GT-193 · [Типографіка: Перевірка шрифтів та еластичність UI] Аудит Segoe UI та автоперенесення тексту",
        "description": "Перевірка покриття Segoe UI (підтримує всю європейську та розширену кирилицю). Перевірка відсутності обрізання тексту в діалогах."
    },
    367: {
        "title": "GT-194 · [Спільнота & QA: Шаблон та інтеграційні тести] Експорт gametrimmer.template.json та тести в cargo test",
        "description": "Генерація коментованого `locales/gametrimmer.template.json` та написання інтеграційного тесту `tests/l10n.rs`, що звіряє наявність усіх ключів."
    },
    368: {
        "title": "GT-195 · [Міграція: Експорт базових en/uk пакетів та повний ретрофіт] Генерація locales/en.json та locales/uk.json",
        "description": "Експорт усіх поточних 214 рядків у `locales/en.json` та `locales/uk.json`, підключення `include_str!` та верифікація 100% тестів."
    }
}

for task_id, data in task_updates.items():
    url = f"http://127.0.0.1:3456/api/v1/tasks/{task_id}"
    req = urllib.request.Request(url, data=json.dumps(data).encode('utf-8'), headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req) as resp:
            print(f"Updated Task {task_id}: {data['title'][:60]}...")
    except Exception as e:
        print(f"Error updating task {task_id}: {e}")

print("\n--- ALL VIKUNJA TASKS UPDATED TO LEAN ARCHITECTURE ---")
