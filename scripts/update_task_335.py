import subprocess
import urllib.request
import json

ps_cmd = """Add-Type -AssemblyName System.Security
$b = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\antigravity-agent.api-token.dpapi")
$u = [System.Security.Cryptography.ProtectedData]::Unprotect($b, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($u)
"""

res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True)
token = res.stdout.strip()

headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

req_get = urllib.request.Request("http://127.0.0.1:3456/api/v2/tasks/335", headers=headers)
with urllib.request.urlopen(req_get) as resp:
    task = json.loads(resp.read().decode("utf-8"))

print("Task title:", task.get("title"))
desc = task.get("description", "")
if "gt-157-step-by-step-implementation-playbook-2026-08-19.md" not in desc:
    add_text = "<p><strong>📘 Детальний Playbook з реалізації кожного кроку:</strong> <code>docs/gt-157-step-by-step-implementation-playbook-2026-08-19.md</code></p>"
    task["description"] = add_text + desc
    req_put = urllib.request.Request("http://127.0.0.1:3456/api/v2/tasks/335", data=json.dumps(task).encode("utf-8"), headers=headers, method="PUT")
    with urllib.request.urlopen(req_put) as resp_p:
        print("Updated Vikunja task 335 successfully with PUT!")


else:
    print("Already up to date.")
