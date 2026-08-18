import os
import sys
import glob
import json
import shutil
import time

def get_dir_size(path):
    total = 0
    count = 0
    try:
        if not os.path.exists(path):
            return 0, 0
        for root, dirs, files in os.walk(path):
            for f in files:
                fp = os.path.join(root, f)
                try:
                    total += os.path.getsize(fp)
                    count += 1
                except (OSError, IOError):
                    pass
    except Exception:
        pass
    return total, count

def format_size(bytes_val):
    if bytes_val < 1024:
        return f"{bytes_val} B"
    elif bytes_val < 1024**2:
        return f"{bytes_val/1024:.2f} KB"
    elif bytes_val < 1024**3:
        return f"{bytes_val/(1024**2):.2f} MB"
    else:
        return f"{bytes_val/(1024**3):.2f} GB"

print("--- STARTING SYSTEM DISK POTENTIAL AUDIT ---")

results = {}

# 1. Steam Downloading & Temp Staging
steam_roots = [
    r"C:\Program Files (x86)\Steam",
    r"C:\Program Files\Steam",
    r"D:\SteamLibrary",
    r"E:\SteamLibrary",
    r"F:\SteamLibrary",
    r"G:\SteamLibrary",
    r"H:\SteamLibrary",
    r"J:\SteamLibrary"
]
downloading_total = 0
downloading_files = 0
downloading_details = []

for sr in steam_roots:
    for sub in ["downloading", "temp"]:
        p = os.path.join(sr, "steamapps", sub)
        if os.path.exists(p):
            sz, cnt = get_dir_size(p)
            if sz > 0:
                downloading_total += sz
                downloading_files += cnt
                downloading_details.append({"path": p, "size": sz, "count": cnt})

results["steam_downloading_temp"] = {
    "total_bytes": downloading_total,
    "total_files": downloading_files,
    "details": downloading_details
}
print(f"1. Steam Downloading & Temp: {format_size(downloading_total)} ({downloading_files} files)")

# 2. Steam Workshop Orphans & Content
workshop_total = 0
workshop_files = 0
workshop_orphans_total = 0
workshop_orphans_files = 0
workshop_details = []

for sr in steam_roots:
    sa = os.path.join(sr, "steamapps")
    wc = os.path.join(sa, "workshop", "content")
    if os.path.exists(wc):
        for appid in os.listdir(wc):
            app_dir = os.path.join(wc, appid)
            if os.path.isdir(app_dir):
                sz, cnt = get_dir_size(app_dir)
                workshop_total += sz
                workshop_files += cnt
                
                # Check if game manifest exists
                manifest_path = os.path.join(sa, f"appmanifest_{appid}.acf")
                is_installed = os.path.exists(manifest_path)
                
                # Check appworkshop manifest
                ws_manifest = os.path.join(sa, "workshop", f"appworkshop_{appid}.acf")
                subscribed_items = set()
                if os.path.exists(ws_manifest):
                    try:
                        with open(ws_manifest, "r", encoding="utf-8", errors="ignore") as f:
                            content = f.read()
                            # simple parser for workshop items
                            import re
                            subscribed_items = set(re.findall(r'"(\d{5,15})"', content))
                    except Exception:
                        pass
                
                app_orphan_bytes = 0
                app_orphan_cnt = 0
                if not is_installed:
                    app_orphan_bytes = sz
                    app_orphan_cnt = cnt
                else:
                    for mod_id in os.listdir(app_dir):
                        mod_path = os.path.join(app_dir, mod_id)
                        if os.path.isdir(mod_path):
                            if subscribed_items and mod_id not in subscribed_items:
                                m_sz, m_cnt = get_dir_size(mod_path)
                                app_orphan_bytes += m_sz
                                app_orphan_cnt += m_cnt
                
                workshop_orphans_total += app_orphan_bytes
                workshop_orphans_files += app_orphan_cnt
                workshop_details.append({
                    "appid": appid,
                    "installed": is_installed,
                    "size": sz,
                    "orphan_bytes": app_orphan_bytes,
                    "orphan_count": app_orphan_cnt
                })

results["steam_workshop"] = {
    "total_bytes": workshop_total,
    "total_orphans_bytes": workshop_orphans_total,
    "total_orphans_files": workshop_orphans_files,
    "details": workshop_details
}
print(f"2. Steam Workshop Content: Total {format_size(workshop_total)}, Confirmed Orphans: {format_size(workshop_orphans_total)} ({workshop_orphans_files} files)")

# 3. Crash Dumps & Diagnostic Logs
local_appdata = os.environ.get("LOCALAPPDATA", "")
appdata = os.environ.get("APPDATA", "")
user_profile = os.environ.get("USERPROFILE", "")

crash_total = 0
crash_files = 0
crash_details = []

# 3a. %LOCALAPPDATA%\CrashDumps
wer_dumps = os.path.join(local_appdata, "CrashDumps")
if os.path.exists(wer_dumps):
    sz, cnt = get_dir_size(wer_dumps)
    crash_total += sz
    crash_files += cnt
    crash_details.append({"category": "Windows WER CrashDumps", "path": wer_dumps, "size": sz, "count": cnt})

# 3b. Unreal Engine Crashes & Logs in LocalAppData and Game Dirs
for root_candidate in [local_appdata, r"F:\SteamLibrary\steamapps\common", r"G:\SteamLibrary\steamapps\common", r"H:\SteamLibrary\steamapps\common"]:
    if os.path.exists(root_candidate):
        try:
            for item in os.listdir(root_candidate):
                subpath = os.path.join(root_candidate, item)
                if os.path.isdir(subpath):
                    for crash_sub in ["Saved\\Crashes", "Saved\\Logs", "Saved\\SaveGames\\Crashes", "Engine\\Saved\\Crashes"]:
                        cp = os.path.join(subpath, crash_sub)
                        if os.path.exists(cp):
                            sz, cnt = get_dir_size(cp)
                            if sz > 0:
                                crash_total += sz
                                crash_files += cnt
                                crash_details.append({"category": "Unreal Engine Crash/Logs", "path": cp, "size": sz, "count": cnt})
        except Exception:
            pass

# 3c. Unity Player.log & LocalLow Crashes
locallow = os.path.join(user_profile, "AppData", "LocalLow")
if os.path.exists(locallow):
    for root, dirs, files in os.walk(locallow):
        for f in files:
            if f.lower() in ["player.log", "player-prev.log"] or f.lower().endswith(".dmp"):
                fp = os.path.join(root, f)
                try:
                    sz = os.path.getsize(fp)
                    crash_total += sz
                    crash_files += 1
                    crash_details.append({"category": "Unity Log/Dump", "path": fp, "size": sz, "count": 1})
                except Exception:
                    pass

results["crash_dumps_logs"] = {
    "total_bytes": crash_total,
    "total_files": crash_files,
    "details": crash_details
}
print(f"3. Game Crash Dumps & Logs: {format_size(crash_total)} ({crash_files} files)")

# 4. Shader Caches (NVIDIA, AMD, DirectX, Steam)
shader_total = 0
shader_files = 0
shader_details = []

shader_paths = [
    ("NVIDIA DXCache", os.path.join(local_appdata, "NVIDIA", "DXCache")),
    ("NVIDIA GLCache", os.path.join(local_appdata, "NVIDIA", "GLCache")),
    ("NVIDIA NV_Cache", os.path.join(local_appdata, "NVIDIA", "NV_Cache")),
    ("NVIDIA ComputeCache", os.path.join(local_appdata, "NVIDIA", "ComputeCache")),
    ("AMD DxCache", os.path.join(local_appdata, "AMD", "DxCache")),
    ("AMD DxcCache", os.path.join(local_appdata, "AMD", "DxcCache")),
    ("DirectX D3DSCache", os.path.join(local_appdata, "D3DSCache")),
]

for name, sp in shader_paths:
    if os.path.exists(sp):
        sz, cnt = get_dir_size(sp)
        if sz > 0:
            shader_total += sz
            shader_files += cnt
            shader_details.append({"name": name, "path": sp, "size": sz, "count": cnt})

# Steam Shadercache
steam_shader_total = 0
steam_shader_files = 0
steam_shader_orphan_total = 0
steam_shader_orphan_files = 0

for sr in steam_roots:
    sa = os.path.join(sr, "steamapps")
    sc = os.path.join(sa, "shadercache")
    if os.path.exists(sc):
        for appid in os.listdir(sc):
            sp = os.path.join(sc, appid)
            if os.path.isdir(sp):
                sz, cnt = get_dir_size(sp)
                steam_shader_total += sz
                steam_shader_files += cnt
                manifest_path = os.path.join(sa, f"appmanifest_{appid}.acf")
                if not os.path.exists(manifest_path):
                    steam_shader_orphan_total += sz
                    steam_shader_orphan_files += cnt

shader_details.append({
    "name": "Steam Shadercache Total",
    "size": steam_shader_total,
    "count": steam_shader_files,
    "orphan_size": steam_shader_orphan_total,
    "orphan_count": steam_shader_orphan_files
})

results["shader_caches"] = {
    "gpu_shader_bytes": shader_total,
    "gpu_shader_files": shader_files,
    "steam_shader_bytes": steam_shader_total,
    "steam_shader_orphan_bytes": steam_shader_orphan_total,
    "details": shader_details
}
print(f"4. GPU Shader Caches: {format_size(shader_total)} ({shader_files} files); Steam Shadercache: {format_size(steam_shader_total)} (Orphans: {format_size(steam_shader_orphan_total)})")

# 5. Save Game Bloat & Autosave Hoarders
save_total = 0
save_files = 0
save_details = []

save_roots = [
    ("Saved Games", os.path.join(user_profile, "Saved Games")),
    ("My Games (Documents)", os.path.join(user_profile, "Documents", "My Games")),
    ("Paradox Interactive", os.path.join(user_profile, "Documents", "Paradox Interactive")),
    ("Baldur's Gate 3 Saves", os.path.join(local_appdata, "Larian Studios", "Baldur's Gate 3", "PlayerProfiles", "Public", "Savegames")),
    ("CD Projekt Red (Cyberpunk/Witcher)", os.path.join(user_profile, "Saved Games", "CD Projekt Red")),
    ("Bethesda Saves (Starfield/Skyrim)", os.path.join(user_profile, "Documents", "My Games", "Starfield")),
]

for name, s_path in save_roots:
    if os.path.exists(s_path):
        sz, cnt = get_dir_size(s_path)
        if sz > 0:
            save_total += sz
            save_files += cnt
            save_details.append({"name": name, "path": s_path, "size": sz, "count": cnt})

results["save_games"] = {
    "total_bytes": save_total,
    "total_files": save_files,
    "details": save_details
}
print(f"5. Save Games & Autosaves: {format_size(save_total)} ({save_files} files)")

# 6. Mod Manager Download Staging & Caches (Vortex, MO2, CurseForge)
mod_total = 0
mod_files = 0
mod_details = []

mod_paths = [
    ("Vortex Downloads (AppData)", os.path.join(appdata, "Vortex", "downloads")),
    ("Vortex AppData", os.path.join(appdata, "Vortex")),
    ("CurseForge Cache", os.path.join(appdata, "CurseForge")),
    ("Overwolf/CurseForge Cache", os.path.join(local_appdata, "Overwolf")),
    ("ModOrganizer AppData", os.path.join(local_appdata, "ModOrganizer")),
]

# Check drives for Vortex Mods / Downloads
for d in ['C:\\', 'D:\\', 'E:\\', 'F:\\', 'G:\\', 'H:\\', 'J:\\']:
    v_dl = os.path.join(d, "Vortex Downloads")
    if os.path.exists(v_dl):
        mod_paths.append((f"Vortex Downloads ({d})", v_dl))
    mo2_dl = os.path.join(d, "ModOrganizer")
    if os.path.exists(mo2_dl):
        mod_paths.append((f"ModOrganizer ({d})", mo2_dl))

for name, mp in mod_paths:
    if os.path.exists(mp):
        sz, cnt = get_dir_size(mp)
        if sz > 0:
            mod_total += sz
            mod_files += cnt
            mod_details.append({"name": name, "path": mp, "size": sz, "count": cnt})

results["mod_managers"] = {
    "total_bytes": mod_total,
    "total_files": mod_files,
    "details": mod_details
}
print(f"6. Mod Manager Downloads & Caches: {format_size(mod_total)} ({mod_files} files)")

# 7. Launcher Webview Caches (CEF, Electron, Chromium)
cef_total = 0
cef_files = 0
cef_details = []

cef_paths = [
    ("Steam HTML Cache", os.path.join(local_appdata, "Steam", "htmlcache")),
    ("Epic Games Launcher Webcache", os.path.join(local_appdata, "EpicGamesLauncher", "Saved", "webcache")),
    ("Ubisoft Connect Cache", os.path.join(local_appdata, "Ubisoft Game Launcher", "cache")),
    ("EA Desktop CEF Cache", os.path.join(local_appdata, "Electronic Arts", "EA Desktop", "CEF")),
    ("Battle.net Web/Agent Cache", os.path.join(local_appdata, "Battle.net")),
    ("Riot Client Cache", os.path.join(local_appdata, "Riot Games", "Riot Client", "Data", "Caches")),
]

for name, cp in cef_paths:
    if os.path.exists(cp):
        sz, cnt = get_dir_size(cp)
        if sz > 0:
            cef_total += sz
            cef_files += cnt
            cef_details.append({"name": name, "path": cp, "size": sz, "count": cnt})

results["launcher_cef_caches"] = {
    "total_bytes": cef_total,
    "total_files": cef_files,
    "details": cef_details
}
print(f"7. Launcher Web/CEF Caches: {format_size(cef_total)} ({cef_files} files)")

# Write results to json
with open("temp/system_audit_results.json", "w", encoding="utf-8") as f:
    json.dump(results, f, indent=2, ensure_ascii=False)

print("--- AUDIT COMPLETE: saved to temp/system_audit_results.json ---")
