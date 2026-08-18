import json
import subprocess
import urllib.request
import sys

sys.stdout.reconfigure(encoding='utf-8')

ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\google-antigravity-agent.api-token.dpapi")
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""
token = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True).stdout.strip()
headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}

all_tasks = []
page = 1
while True:
    req = urllib.request.Request(f"http://127.0.0.1:3456/api/v1/projects/5/tasks?page={page}&per_page=50", headers=headers)
    with urllib.request.urlopen(req) as r:
        batch = json.loads(r.read().decode('utf-8'))
        if not batch:
            break
        all_tasks.extend(batch)
        page += 1

open_tasks = [t for t in all_tasks if not t.get('done', False)]
print(f"Total open tasks in Project 5: {len(open_tasks)}")
for t in sorted(open_tasks, key=lambda x: x.get('id', 0)):
    print(f"ID {t.get('id')}: [{t.get('identifier', '')}] {t.get('title')}")
