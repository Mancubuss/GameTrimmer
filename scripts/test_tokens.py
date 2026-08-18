import subprocess
import urllib.request
import json
import os

files = ['google-antigravity-agent.api-token.dpapi', 'antigravity-agent.api-token.dpapi', 'codex-bot.api-token.dpapi', 'claude-code-agent.api-token.dpapi']
for f in files:
    p = os.path.join(r"E:\Mancubus\Projects\Vibecoding\Vikunja", f)
    if not os.path.exists(p):
        continue
    ps_cmd = f"""
    Add-Type -AssemblyName System.Security
    $bytes = [System.IO.File]::ReadAllBytes("{p}")
    $unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
    [System.Text.Encoding]::UTF8.GetString($unprotected)
    """
    try:
        res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True)
        token = res.stdout.strip()
        req = urllib.request.Request("http://127.0.0.1:3456/api/v1/projects", headers={"Authorization": f"Bearer {token}"})
        with urllib.request.urlopen(req) as resp:
            u = json.loads(resp.read().decode('utf-8'))
            print(f"{f}: SUCCESS (projects count={len(u)})")
    except Exception as e:
        print(f"{f}: FAIL ({e})")
