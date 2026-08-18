import subprocess
import urllib.request
import json
import sys

sys.stdout.reconfigure(encoding='utf-8')

ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\antigravity-agent.api-token.dpapi")
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""

res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True)
token = res.stdout.strip()
headers = {'Authorization': f'Bearer {token}'}

req = urllib.request.Request('http://127.0.0.1:3456/api/v2/projects/5/views/24/buckets', headers=headers)
try:
    with urllib.request.urlopen(req) as r:
        buckets = json.loads(r.read().decode('utf-8'))
        print("Buckets:", json.dumps(buckets, ensure_ascii=False, indent=2))
except Exception as e:
    print("Error:", e)
