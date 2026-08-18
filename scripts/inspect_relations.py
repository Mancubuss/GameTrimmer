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
headers = {'Authorization': f'Bearer {token}', 'Accept': 'application/json'}

def inspect_task(tid):
    req = urllib.request.Request(f'http://127.0.0.1:3456/api/v1/tasks/{tid}', headers=headers)
    with urllib.request.urlopen(req) as resp:
        t = json.loads(resp.read().decode('utf-8'))
        print(f"\n--- Task {tid}: {t.get('title')} ---")
        print("related_tasks:", json.dumps(t.get("related_tasks"), ensure_ascii=False, indent=2))
        print("parent_task_id:", t.get("parent_task_id"))

inspect_task(321) # EP11
inspect_task(336) # EP13
inspect_task(344) # EP14
