import subprocess
import urllib.request
import json
import os

def get_dpapi(path):
    clean_path = os.path.abspath(path)
    ps_cmd = f"""
    Add-Type -AssemblyName System.Security
    $b = [System.IO.File]::ReadAllBytes("{clean_path}")
    $u = [System.Security.Cryptography.ProtectedData]::Unprotect($b, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
    [System.Text.Encoding]::UTF8.GetString($u)
    """
    return subprocess.run(['powershell', '-NoProfile', '-Command', ps_cmd], capture_output=True, text=True, check=True).stdout.strip()

pwd = get_dpapi('E:/Mancubus/Projects/Vibecoding/Vikunja/claude-code-agent.password.dpapi')
print('Got password length:', len(pwd))

# Try logging in
req = urllib.request.Request('http://127.0.0.1:3456/api/v1/login', data=json.dumps({'username': 'claude-code-agent', 'password': pwd}).encode('utf-8'), headers={'Content-Type': 'application/json'})
try:
    with urllib.request.urlopen(req) as resp:
        data = json.loads(resp.read().decode('utf-8'))
        print('Login success! Token:', data.get('token')[:30] + '...')
        # Save or use this session token
        session_token = data.get('token')
        
        # Test getting user or tasks
        req2 = urllib.request.Request('http://127.0.0.1:3456/api/v1/user', headers={'Authorization': f'Bearer {session_token}'})
        with urllib.request.urlopen(req2) as resp2:
            print('User:', resp2.read().decode('utf-8'))
except Exception as e:
    print('Login error:', e)
