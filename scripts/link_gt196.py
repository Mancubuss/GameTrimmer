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
headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

rel_url = f"http://127.0.0.1:3456/api/v1/tasks/360/relations"
rel_payload = json.dumps({
    "other_task_id": 369,
    "relation_kind": "subtask"
}).encode('utf-8')
rel_req = urllib.request.Request(rel_url, data=rel_payload, headers=headers, method="PUT")

with urllib.request.urlopen(rel_req) as resp:
    print(f"Linked task 369 as subtask of Epic 360: {resp.read().decode('utf-8')}")
