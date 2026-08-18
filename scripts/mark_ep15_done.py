import subprocess
import requests
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

dpapi_path = r"E:\Mancubus\Projects\Vibecoding\Vikunja\antigravity-agent.api-token.dpapi"
ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes('E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\antigravity-agent.api-token.dpapi')
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""
res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True)
token = res.stdout.strip()
if not token:
    print(f"Failed to decrypt Vikunja token! Stderr: {res.stderr}")
    sys.exit(1)

base_url = "http://127.0.0.1:3456/api/v1"
headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json"
}

task_ids = [353, 354, 355, 356, 357, 358, 359, 371, 352]

for tid in task_ids:
    # Update task done = True and bucket_id = 18 (Done)
    r = requests.post(f"{base_url}/tasks/{tid}", headers=headers, json={"done": True, "bucket_id": 18})
    if r.status_code == 200:
        print(f"Task {tid} marked as DONE.")
    else:
        print(f"Task {tid} update failed: {r.status_code} {r.text}")

print("All Vikunja tasks for Epic 15 marked as DONE!")
