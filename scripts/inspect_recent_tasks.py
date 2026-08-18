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
headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}

for tid in range(350, 375):
    try:
        req = urllib.request.Request(f"http://127.0.0.1:3456/api/v1/tasks/{tid}", headers=headers)
        with urllib.request.urlopen(req) as r:
            t = json.loads(r.read().decode('utf-8'))
            print(f"ID {t.get('id')}: done={t.get('done')}, bucket={t.get('bucket_id')} | {t.get('title')}")
    except Exception as e:
        pass
