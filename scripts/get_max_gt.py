import subprocess
import urllib.request
import json
import re
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

def get_dpapi(path):
    clean_path = os.path.abspath(path)
    ps_cmd = f"""
    Add-Type -AssemblyName System.Security
    $b = [System.IO.File]::ReadAllBytes("{clean_path}")
    $u = [System.Security.Cryptography.ProtectedData]::Unprotect($b, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
    [System.Text.Encoding]::UTF8.GetString($u)
    """
    return subprocess.run(['powershell', '-NoProfile', '-Command', ps_cmd], capture_output=True, text=True, check=True).stdout.strip()

token = get_dpapi('E:/Mancubus/Projects/Vibecoding/Vikunja/antigravity-agent.api-token.dpapi')

headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

page = 1
max_gt = 0
all_gt = []
while True:
    req = urllib.request.Request(f'http://127.0.0.1:3456/api/v1/projects/5/tasks?page={page}&per_page=100', headers=headers)
    with urllib.request.urlopen(req) as resp:
        res = json.loads(resp.read().decode('utf-8'))
    if not res:
        break
    for item in res:
        title = item.get('title', '')
        m = re.search(r'GT-(\d+)', title)
        if m:
            num = int(m.group(1))
            all_gt.append((num, item.get('id'), title))
            if num > max_gt:
                max_gt = num
    page += 1

print(f"Max GT number: GT-{max_gt}")
print("Top 15 highest GT numbers:")
for num, tid, title in sorted(all_gt, key=lambda x: x[0], reverse=True)[:15]:
    print(f"  GT-{num} (task id {tid}): {title}")
