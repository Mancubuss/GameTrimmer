import json
import subprocess
import urllib.request
import urllib.error
import sys

sys.stdout.reconfigure(encoding='utf-8')

ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\google-antigravity-agent.api-token.dpapi")
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""

token = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True).stdout.strip()
headers = {
    'Authorization': f'Bearer {token}',
    'Content-Type': 'application/json; charset=utf-8',
    'Accept': 'application/json'
}

epic_id = 344 # GT-EP14
child_ids = [345, 346, 347, 348, 349, 350, 351] # GT-174..GT-180

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
            print(f"Linked task {cid} as subtask of epic {epic_id}: {res}")
    except urllib.error.HTTPError as e:
        err = e.read().decode('utf-8')
        print(f"Error linking {cid}: HTTP {e.code} - {err}")
