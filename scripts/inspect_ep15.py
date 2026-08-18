import subprocess
import urllib.request
import json
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

# 1. Get task 352 (Epic 15)
req = urllib.request.Request("http://127.0.0.1:3456/api/v1/tasks/352", headers=headers)
with urllib.request.urlopen(req) as r:
    task = json.loads(r.read().decode('utf-8'))
    print("\n=== EPIC 15 (Task 352) ===")
    print(f"Title: {task.get('title')}")
    print(f"Done: {task.get('done')}, Bucket ID: {task.get('bucket_id')}")
    print("\n=== RELATED TASKS ===")
    for rel_type, rel_list in task.get('related_tasks', {}).items():
        print(f"Relation type '{rel_type}':")
        for item in rel_list:
            print(f"  - [{item.get('id')}] {item.get('title')} (Done: {item.get('done')}, Bucket: {item.get('bucket_id')}, Priority: {item.get('priority')})")

# 2. Check tasks 353 to 359
print("\n=== INSPECTING INDIVIDUAL TASKS 353-359 ===")
for tid in range(353, 360):
    try:
        req_t = urllib.request.Request(f"http://127.0.0.1:3456/api/v1/tasks/{tid}", headers=headers)
        with urllib.request.urlopen(req_t) as rt:
            t = json.loads(rt.read().decode('utf-8'))
            print(f"[{t.get('id')}] {t.get('title')} | Done: {t.get('done')} | Bucket: {t.get('bucket_id')} | Priority: {t.get('priority')}")
    except Exception as e:
        print(f"Task {tid} error: {e}")
