import subprocess
import urllib.request
import json
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

def get_dpapi(path):
    clean_path = os.path.abspath(path)
    ps_cmd = f"""
    Add-Type -AssemblyName System.Security
    $b = [System.IO.File]::ReadAllBytes("{clean_path}")
    $u = [System.Security.Cryptography.ProtectedData]::Unprotect($b, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
    [System.Text.Encoding]::UTF8.GetString($u)
    """
    return subprocess.run(['powershell', '-NoProfile', '-Command', ps_cmd], capture_output=True, text=True, check=True).stdout.strip()

token = get_dpapi('E:/Mancubus/Projects/Vibecoding/Vikunja/antigravity-agent.api-token.dpapi')

headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

project_id = 5
epic_id = 352

task_data = {
    "title": "GT-198 · [Локалізація] Повна локалізація нових категорій, діагностики та елементів інтерфейсу Епіку 15 для 30 мовних пакетів",
    "description": """# GT-198: Повна локалізація нових категорій очищення та UI-компонентів Епіку 15

- **Батьківський епік:** [GT-EP15 (id: 352)](http://127.0.0.1:3456) · «Розширення каталогу очищення: Воркшоп, незавершені завантаження, шейдери, дампи та ігрові кеші»
- **Пріоритет:** High (3)
- **Призначений субагент:** `L10n-Janitor`

---

### Мета завдання:
Своєчасне та повне додавання всіх нових ключів локалізації, описів категорій, повідомлень діагностики та елементів інтерфейсу (Smart Save Pruner, Shader Cache, Workshop Orphan Cleaner, CEF Webview Caches, Crash Dumps) у всі 30 підтримуваних мовних пакетів GameTrimmer.

### Критерії приймання (Acceptance Criteria):
1. Всі нові ключі локалізації оголошені в `crates/app/src/i18n/` з еталонними англійськими та українськими рядками.
2. Оновлено `locales/gametrimmer.template.json` та `locales/*.json` для всіх 30 мов без пропущених ключів.
3. Оновлено вбудовані fallback-структури (`EMBEDDED_LOCALE_EN`, `EMBEDDED_LOCALE_UK`).
4. Тести `cargo test --test l10n` проходять з оцінкою 100% відповідності ключів.
5. Відсутність сирих нелокалізованих ключів у GUI.
""",
    "priority": 3,
    "bucket_id": 16 # "До виконання"
}

# 1. Create task
req_create = urllib.request.Request(
    f"http://127.0.0.1:3456/api/v1/projects/{project_id}/tasks",
    data=json.dumps(task_data).encode('utf-8'),
    headers=headers,
    method="PUT"
)

with urllib.request.urlopen(req_create) as resp:
    created = json.loads(resp.read().decode('utf-8'))
    new_task_id = created.get('id')
    print(f"Created task ID: {new_task_id}, Title: {created.get('title')}")

# 2. Add subtask relation to Epic 15 (task 352)
rel_data = {
    "other_task_id": new_task_id,
    "relation_kind": "subtask"
}

req_rel = urllib.request.Request(
    f"http://127.0.0.1:3456/api/v1/tasks/{epic_id}/relations",
    data=json.dumps(rel_data).encode('utf-8'),
    headers=headers,
    method="PUT"
)

with urllib.request.urlopen(req_rel) as resp:
    rel_res = json.loads(resp.read().decode('utf-8'))
    print(f"Linked task {new_task_id} as subtask to Epic {epic_id}: {rel_res}")
