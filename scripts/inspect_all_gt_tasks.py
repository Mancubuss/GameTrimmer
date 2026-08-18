import json
import subprocess
import urllib.request
import urllib.error
import sys

sys.stdout.reconfigure(encoding='utf-8')

# Get decrypted token
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
    "Accept": "application/json"
}

# 1. Fetch views / buckets
req_views = urllib.request.Request("http://127.0.0.1:3456/api/v1/projects/5/views", headers=headers)
with urllib.request.urlopen(req_views) as r:
    views = json.loads(r.read().decode('utf-8'))

kanban_view = None
for v in views:
    if v.get('view_kind') == 'kanban' or v.get('view_type') == 'kanban':
        kanban_view = v
        break
if not kanban_view and views:
    kanban_view = views[0]

print("Views:", views)
view_id = kanban_view['id'] if kanban_view else None

if view_id:
    req_buckets = urllib.request.Request(f"http://127.0.0.1:3456/api/v1/projects/5/views/{view_id}/buckets", headers=headers)
    with urllib.request.urlopen(req_buckets) as r:
        buckets = json.loads(r.read().decode('utf-8'))
    print("Buckets in view:", [(b.get('id'), b.get('title')) for b in buckets])

# 2. Fetch tasks
req_tasks = urllib.request.Request("http://127.0.0.1:3456/api/v1/projects/5/tasks?per_page=200", headers=headers)
with urllib.request.urlopen(req_tasks) as r:
    tasks = json.loads(r.read().decode('utf-8'))

print(f"\nTotal tasks in Project 5: {len(tasks)}")
open_tasks = [t for t in tasks if not t.get('done', False)]
print(f"Open tasks count: {len(open_tasks)}")
for t in open_tasks:
    print(f"  ID {t.get('id')}: [{t.get('identifier', '')}] {t.get('title')} (bucket: {t.get('bucket_id')})")
