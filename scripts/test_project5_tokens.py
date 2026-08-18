import subprocess
import urllib.request
import glob
import os
import json

files = glob.glob('E:/Mancubus/Projects/Vibecoding/Vikunja/*.dpapi')
for f in files:
    clean_path = os.path.abspath(f)
    ps_cmd = f"""
    Add-Type -AssemblyName System.Security
    $b = [System.IO.File]::ReadAllBytes("{clean_path}")
    $u = [System.Security.Cryptography.ProtectedData]::Unprotect($b, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
    [System.Text.Encoding]::UTF8.GetString($u)
    """
    try:
        res = subprocess.run(['powershell', '-NoProfile', '-Command', ps_cmd], capture_output=True, text=True, check=True)
        token = res.stdout.strip()
        req = urllib.request.Request('http://127.0.0.1:3456/api/v1/projects/5/tasks', headers={'Authorization': f'Bearer {token}'})
        with urllib.request.urlopen(req) as resp:
            data = resp.read().decode('utf-8')
            tasks = json.loads(data)
            print(f'SUCCESS for {os.path.basename(f)}: project 5 returned {len(tasks)} tasks!')
    except Exception as e:
        print(f'FAIL for {os.path.basename(f)}: {e}')
