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

project_id = 5

task_data = {
    "title": "GT-197 · [Оптимізація] Вбудовування офіційних пакетів мов Steam у бінарник (Zero-Files Distro & In-Memory Locale Bundle)",
    "description": """# GT-197: Вбудовування офіційних пакетів мов Steam у бінарник (Zero-Files Distro & In-Memory Locale Bundle)

- **Батьківський епік:** GT-EP16 (ID 360)
- **Статус:** Заплановано / Backlog (Очікує свого часу)
- **Мета:** Повне усунення візуального шуму теки `locales/` (30+ окремих файлів) із портативного релізного дистрибутиву шляхом вшивання 28 офіційних мов Steam безпосередньо у виконуваний файл `gametrimmer.exe`.

---

### Архітектурні деталі та план реалізації:
1. **Compile-Time Baking:**
   - Вшивання офіційних JSON-файлів мов у бінарник через `include_str!` або стиснені байтові зрізи (`include_bytes!` + компресія zlib/deflate ~42 КБ оверхеду на весь бінарник).
2. **Чистий портативний дистрибутив (Zero Loose Files):**
   - Утиліта поширюється у вигляді одного чистого `gametrimmer.exe` без необхідності тягнути поруч теку `locales/` із десятками неактивних файлів.
3. **Збереження гнучкості для спільноти (Cascading External Override):**
   - Якщо користувач або перекладач створює локальну теку `locales/` і кладе туди файл (наприклад, `locales/pl.json` або `locales/my_lang.json`), рушій програми в першу чергу читає зовнішній файл, перекриваючи вбудовані ресурси.
4. **Миттєве завантаження:**
   - 0 дискових I/O операцій під час старту та перемикання будь-якої з 28 вбудованих мов.
"""
}

# Create task in project 5
url = f"http://127.0.0.1:3456/api/v1/projects/{project_id}/tasks"
payload = json.dumps(task_data).encode('utf-8')
req = urllib.request.Request(url, data=payload, headers=headers, method="PUT")

with urllib.request.urlopen(req) as resp:
    created = json.loads(resp.read().decode('utf-8'))
    task_id = created.get("id")
    print(f"Created task {task_id}: {created.get('title')}")

# Link as subtask to Epic 360
rel_url = f"http://127.0.0.1:3456/api/v1/tasks/360/relations"
rel_payload = json.dumps({
    "other_task_id": task_id,
    "relation_kind": "subtask"
}).encode('utf-8')
rel_req = urllib.request.Request(rel_url, data=rel_payload, headers=headers, method="PUT")

with urllib.request.urlopen(rel_req) as resp:
    print(f"Linked task {task_id} as subtask of Epic 360: {resp.read().decode('utf-8')}")
