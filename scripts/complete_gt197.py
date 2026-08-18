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
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

# Task 370 (GT-197)
url = "http://127.0.0.1:3456/api/v1/tasks/370"
payload = json.dumps({"done": True}).encode('utf-8')
req = urllib.request.Request(url, data=payload, headers=headers, method="POST")

with urllib.request.urlopen(req) as resp:
    data = json.loads(resp.read().decode('utf-8'))
    print(f"Task 370 ({data.get('title')}) marked as DONE (done={data.get('done')})")
