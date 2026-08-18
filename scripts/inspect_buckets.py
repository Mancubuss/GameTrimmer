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

req_views = urllib.request.Request("http://127.0.0.1:3456/api/v1/projects/5/views", headers=headers)
with urllib.request.urlopen(req_views) as r:
    views = json.loads(r.read().decode('utf-8'))
    for v in views:
        view_id = v.get('id')
        print(f"View {view_id}: {v.get('title')} ({v.get('view_kind')})")
        if v.get('bucket_configuration_mode') != 'none':
            try:
                req_b = urllib.request.Request(f"http://127.0.0.1:3456/api/v1/projects/5/views/{view_id}/buckets", headers=headers)
                with urllib.request.urlopen(req_b) as rb:
                    buckets = json.loads(rb.read().decode('utf-8'))
                    for b in buckets:
                        print(f"   Bucket ID: {b.get('id')}, Title: {b.get('title')}")
            except Exception as e:
                print(f"   Bucket error: {e}")
