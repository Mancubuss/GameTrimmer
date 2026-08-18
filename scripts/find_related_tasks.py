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

req = urllib.request.Request("http://127.0.0.1:3456/api/v1/projects/5/tasks?per_page=250", headers=headers)
with urllib.request.urlopen(req) as r:
    tasks = json.loads(r.read().decode('utf-8'))

for t in tasks:
    title = t.get('title', '')
    desc = t.get('description', '')
    text = (title + " " + desc).lower()
    if any(k in text for k in ['watch', 'монітор', 'фонов', 'daemon', 'локалізац', 'ep11', 'ep12', 'ep16']):
        print(f"ID {t.get('id')}: done={t.get('done')} | {title}")
