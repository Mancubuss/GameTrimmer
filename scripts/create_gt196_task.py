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
    "title": "GT-196 · [Локалізація] Повний пакет перекладів для всіх офіційних мов Steam (28+ мов)",
    "description": """# GT-196: Повний пакет перекладів для всіх офіційних мов Steam (28+ мов)

- **Батьківський епік:** GT-EP16 (ID 360)
- **Статус:** В роботі / Реалізація
- **Мета:** Створення готових початкових JSON-пакетів локалізації для всіх мов, підтримуваних платформою Steam, що забезпечує глобальне охоплення аудиторії "з коробки" та служить фундаментом для подальшого доопрацювання спільнотою.

---

### Список мов Steam, що додаються (`locales/*.json`):
1. **de** — Deutsch (German)
2. **fr** — Français (French)
3. **it** — Italiano (Italian)
4. **es** — Español (Spanish - Spain)
5. **es-419** — Español (Spanish - Latin America)
6. **pl** — Polski (Polish)
7. **pt-BR** — Português-Brasil (Portuguese - Brazil)
8. **pt** — Português (Portuguese - Portugal)
9. **ja** — 日本語 (Japanese)
10. **ko** — 한국어 (Korean)
11. **zh-Hans** — 简体中文 (Simplified Chinese)
12. **zh-Hant** — 繁體中文 (Traditional Chinese)
13. **tr** — Türkçe (Turkish)
14. **cs** — Čeština (Czech)
15. **nl** — Nederlands (Dutch)
16. **sv** — Svenska (Swedish)
17. **hu** — Magyar (Hungarian)
18. **da** — Dansk (Danish)
19. **no** — Norsk (Norwegian)
20. **fi** — Suomi (Finnish)
21. **el** — Ελληνικά (Greek)
22. **ro** — Română (Romanian)
23. **bg** — Български (Bulgarian)
24. **vi** — Tiếng Việt (Vietnamese)
25. **th** — ไทย (Thai)
26. **ar** — العربية (Arabic)
27. **ru** — Русский (Russian)
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

# Link as subtask / relation to Epic 360
rel_url = f"http://127.0.0.1:3456/api/v1/tasks/360/relations"
rel_payload = json.dumps({
    "other_task_id": task_id,
    "relation_kind": "parentchild"
}).encode('utf-8')
rel_req = urllib.request.Request(rel_url, data=rel_payload, headers=headers, method="PUT")

try:
    with urllib.request.urlopen(rel_req) as resp:
        print(f"Linked task {task_id} as subtask to Epic 360")
except Exception as e:
    print(f"Relation link response: {e}")
