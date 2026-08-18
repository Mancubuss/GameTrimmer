import os
import sys
import glob
import json
import re

def get_dir_size_and_details(path):
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

print("==================================================")
print("COMPREHENSIVE GAMETRIMMER SYSTEM AUDIT (ACCURATE)")
print("==================================================")

report = {}

# 1. GPU Shader Caches & Driver Bloat
gpu_caches = [
    ("NVIDIA DXCache", r"C:\Users\Mancubus\AppData\Local\NVIDIA\DXCache"),
    ("NVIDIA GLCache", r"C:\Users\Mancubus\AppData\Local\NVIDIA\GLCache"),
    ("NVIDIA NV_Cache", r"C:\Users\Mancubus\AppData\Local\NVIDIA\NV_Cache"),
    ("NVIDIA ComputeCache", r"C:\Users\Mancubus\AppData\Local\NVIDIA\ComputeCache"),
    ("AMD DxCache", r"C:\Users\Mancubus\AppData\Local\AMD\DxCache"),
    ("DirectX D3DSCache", r"C:\Users\Mancubus\AppData\Local\D3DSCache"),
]
gpu_total = 0
gpu_items = []
for name, p in gpu_caches:
    sz, cnt = get_dir_size_and_details(p)
    gpu_total += sz
    if sz > 0:
        gpu_items.append({"name": name, "path": p, "size": sz, "size_str": format_size(sz), "files": cnt})
        print(f"[GPU Shader Cache] {name}: {format_size(sz)} ({cnt} files)")

report["gpu_shader_caches"] = {"total_bytes": gpu_total, "total_str": format_size(gpu_total), "items": gpu_items}

# 2. Steam Shader Caches
steam_libs = [r"F:\SteamLibrary", r"G:\SteamLibrary", r"H:\SteamLibrary", r"C:\Program Files (x86)\Steam"]
steam_sc_total = 0
steam_sc_orphan = 0
steam_sc_items = []

for lib in steam_libs:
    sc = os.path.join(lib, "steamapps", "shadercache")
    if os.path.exists(sc):
        for appid in os.listdir(sc):
            sp = os.path.join(sc, appid)
            if os.path.isdir(sp):
                sz, cnt = get_dir_size_and_details(sp)
                steam_sc_total += sz
                manifest = os.path.join(lib, "steamapps", f"appmanifest_{appid}.acf")
                installed = os.path.exists(manifest)
                if not installed:
                    steam_sc_orphan += sz
                steam_sc_items.append({
                    "appid": appid,
                    "library": lib,
                    "size": sz,
                    "size_str": format_size(sz),
                    "installed": installed
                })

print(f"[Steam Shader Cache] Total: {format_size(steam_sc_total)}, Uninstalled Orphans: {format_size(steam_sc_orphan)}")
report["steam_shader_caches"] = {
    "total_bytes": steam_sc_total,
    "total_str": format_size(steam_sc_total),
    "orphan_bytes": steam_sc_orphan,
    "orphan_str": format_size(steam_sc_orphan),
    "items": steam_sc_items
}

# 3. Steam Staging & Downloading
steam_dl_total = 0
steam_dl_items = []
for lib in steam_libs:
    for sub in ["downloading", "temp"]:
        p = os.path.join(lib, "steamapps", sub)
        if os.path.exists(p):
            sz, cnt = get_dir_size_and_details(p)
            if sz > 0:
                steam_dl_total += sz
                steam_dl_items.append({"path": p, "size": sz, "size_str": format_size(sz), "files": cnt})
                print(f"[Steam Staging] {p}: {format_size(sz)} ({cnt} files)")

report["steam_staging_downloads"] = {
    "total_bytes": steam_dl_total,
    "total_str": format_size(steam_dl_total),
    "items": steam_dl_items
}

# 4. Steam Workshop Content & Orphans
ws_total = 0
ws_orphan_total = 0
ws_items = []

for lib in steam_libs:
    wc = os.path.join(lib, "steamapps", "workshop", "content")
    if os.path.exists(wc):
        for appid in os.listdir(wc):
            ap = os.path.join(wc, appid)
            if os.path.isdir(ap):
                sz, cnt = get_dir_size_and_details(ap)
                ws_total += sz
                manifest = os.path.join(lib, "steamapps", f"appmanifest_{appid}.acf")
                installed = os.path.exists(manifest)
                orphan_sz = sz if not installed else 0
                ws_orphan_total += orphan_sz
                ws_items.append({
                    "appid": appid,
                    "library": lib,
                    "size": sz,
                    "size_str": format_size(sz),
                    "files": cnt,
                    "installed": installed
                })
                print(f"[Steam Workshop] AppID {appid} ({lib}): {format_size(sz)} (Installed: {installed})")

report["steam_workshop"] = {
    "total_bytes": ws_total,
    "total_str": format_size(ws_total),
    "orphan_bytes": ws_orphan_total,
    "orphan_str": format_size(ws_orphan_total),
    "items": ws_items
}

# 5. Game Crash Dumps & Diagnostics Logs
crash_total = 0
crash_items = []

# 5a. WER Dumps
wer = r"C:\Users\Mancubus\AppData\Local\CrashDumps"
if os.path.exists(wer):
    sz, cnt = get_dir_size_and_details(wer)
    if sz > 0:
        crash_total += sz
        crash_items.append({"name": "Windows WER CrashDumps", "path": wer, "size": sz, "size_str": format_size(sz), "files": cnt})
        print(f"[Crash Dumps] Windows WER: {format_size(sz)} ({cnt} files)")

# 5b. Unreal Engine in AppData\Local
local_appdata = r"C:\Users\Mancubus\AppData\Local"
for item in os.listdir(local_appdata):
    sub = os.path.join(local_appdata, item)
    if os.path.isdir(sub):
        for crash_pattern in ["Saved\\Crashes", "Saved\\Logs", "Saved\\SaveGames\\Crashes"]:
            cp = os.path.join(sub, crash_pattern)
            if os.path.exists(cp):
                sz, cnt = get_dir_size_and_details(cp)
                if sz > 0:
                    crash_total += sz
                    crash_items.append({"name": f"UE {item} {crash_pattern}", "path": cp, "size": sz, "size_str": format_size(sz), "files": cnt})
                    print(f"[Crash Dumps/Logs] {item} {crash_pattern}: {format_size(sz)} ({cnt} files)")

# 5c. Unity Player.log
locallow = r"C:\Users\Mancubus\AppData\LocalLow"
if os.path.exists(locallow):
    for root, dirs, files in os.walk(locallow):
        for f in files:
            if f.lower() in ["player.log", "player-prev.log"]:
                fp = os.path.join(root, f)
                try:
                    sz = os.path.getsize(fp)
                    if sz > 0:
                        crash_total += sz
                        crash_items.append({"name": f"Unity {f}", "path": fp, "size": sz, "size_str": format_size(sz), "files": 1})
                except Exception:
                    pass

report["crash_dumps_logs"] = {
    "total_bytes": crash_total,
    "total_str": format_size(crash_total),
    "items": crash_items
}

# 6. Save Games & Autosaves in Documents & Saved Games
save_total = 0
save_items = []
save_roots = [
    (r"E:\Mancubus\Saved Games", "Saved Games (E:)"),
    (r"E:\Mancubus\Documents\My Games", "My Games (E:)"),
    (r"E:\Mancubus\Documents\Paradox Interactive", "Paradox Interactive (E:)"),
    (r"C:\Users\Mancubus\AppData\Local\Larian Studios", "Larian Studios (AppData)"),
    (r"C:\Users\Mancubus\Saved Games", "Saved Games (C:)"),
    (r"C:\Users\Mancubus\Documents\My Games", "My Games (C:)"),
]

for p, label in save_roots:
    if os.path.exists(p):
        for item in os.listdir(p):
            sub = os.path.join(p, item)
            if os.path.isdir(sub):
                sz, cnt = get_dir_size_and_details(sub)
                if sz > 0:
                    save_total += sz
                    save_items.append({"game": item, "location": label, "path": sub, "size": sz, "size_str": format_size(sz), "files": cnt})
                    print(f"[Save Game] {label} / {item}: {format_size(sz)} ({cnt} files)")

report["save_games"] = {
    "total_bytes": save_total,
    "total_str": format_size(save_total),
    "items": save_items
}

# 7. Launcher Web & CEF Caches
cef_total = 0
cef_items = []
cef_paths = [
    ("Steam HTML Cache", r"C:\Users\Mancubus\AppData\Local\Steam\htmlcache"),
    ("Epic Games Launcher Webcache", r"C:\Users\Mancubus\AppData\Local\EpicGamesLauncher\Saved\webcache"),
    ("Ubisoft Connect Cache", r"C:\Users\Mancubus\AppData\Local\Ubisoft Game Launcher\cache"),
    ("EA Desktop CEF", r"C:\Users\Mancubus\AppData\Local\Electronic Arts\EA Desktop\CEF"),
    ("Battle.net Cache", r"C:\Users\Mancubus\AppData\Local\Battle.net"),
    ("Riot Client Cache", r"C:\Users\Mancubus\AppData\Local\Riot Games\Riot Client\Data\Caches"),
    ("GOG Galaxy Web Cache", r"C:\ProgramData\GOG.com\Galaxy\webcache")
]

for name, p in cef_paths:
    if os.path.exists(p):
        sz, cnt = get_dir_size_and_details(p)
        if sz > 0:
            cef_total += sz
            cef_items.append({"name": name, "path": p, "size": sz, "size_str": format_size(sz), "files": cnt})
            print(f"[Launcher Web Cache] {name}: {format_size(sz)} ({cnt} files)")

report["launcher_cef_caches"] = {
    "total_bytes": cef_total,
    "total_str": format_size(cef_total),
    "items": cef_items
}

# 8. Mod Managers (Vortex / MO2 / CurseForge)
mod_total = 0
mod_items = []
mod_paths = [
    ("Vortex AppData", r"C:\Users\Mancubus\AppData\Roaming\Vortex"),
    ("Vortex Downloads", r"E:\Vortex Downloads"),
    ("CurseForge AppData", r"C:\Users\Mancubus\AppData\Roaming\CurseForge"),
    ("Overwolf LocalAppData", r"C:\Users\Mancubus\AppData\Local\Overwolf"),
    ("MO2 AppData", r"C:\Users\Mancubus\AppData\Local\ModOrganizer"),
]
for d in ['C:\\', 'D:\\', 'E:\\', 'F:\\', 'G:\\', 'H:\\']:
    mo_path = os.path.join(d, "ModOrganizer")
    if os.path.exists(mo_path):
        mod_paths.append((f"ModOrganizer ({d})", mo_path))

for name, p in mod_paths:
    if os.path.exists(p):
        sz, cnt = get_dir_size_and_details(p)
        if sz > 0:
            mod_total += sz
            mod_items.append({"name": name, "path": p, "size": sz, "size_str": format_size(sz), "files": cnt})
            print(f"[Mod Manager] {name}: {format_size(sz)} ({cnt} files)")

report["mod_managers"] = {
    "total_bytes": mod_total,
    "total_str": format_size(mod_total),
    "items": mod_items
}

# 9. Large Cutscenes & Video Assets in Installed Games (Bik / Mp4 / Webm)
video_total = 0
video_items = []
for lib in steam_libs:
    common = os.path.join(lib, "steamapps", "common")
    if os.path.exists(common):
        for game in os.listdir(common):
            gp = os.path.join(common, game)
            if os.path.isdir(gp):
                game_vid_sz = 0
                game_vid_cnt = 0
                for root, dirs, files in os.walk(gp):
                    for f in files:
                        ext = os.path.splitext(f)[1].lower()
                        if ext in [".bik", ".bk2", ".mp4", ".webm", ".mkv", ".usm", ".wmv"]:
                            vp = os.path.join(root, f)
                            try:
                                vsz = os.path.getsize(vp)
                                game_vid_sz += vsz
                                game_vid_cnt += 1
                            except Exception:
                                pass
                if game_vid_sz > 500 * (1024**2): # > 500 MB of video
                    video_total += game_vid_sz
                    video_items.append({
                        "game": game,
                        "library": lib,
                        "video_size": game_vid_sz,
                        "video_size_str": format_size(game_vid_sz),
                        "video_files": game_vid_cnt
                    })
                    print(f"[Game Video Assets] {game}: {format_size(game_vid_sz)} in {game_vid_cnt} video files")

report["game_video_assets"] = {
    "total_bytes": video_total,
    "total_str": format_size(video_total),
    "items": video_items
}

with open("temp/comprehensive_system_audit.json", "w", encoding="utf-8") as f:
    json.dump(report, f, indent=2, ensure_ascii=False)

print("==================================================")
print("TOTAL AUDITED BLOAT CHANNELS ON THIS MACHINE:")
print(f"1. GPU Shader Caches:           {report['gpu_shader_caches']['total_str']}")
print(f"2. Steam Shader Cache:          {report['steam_shader_caches']['total_str']}")
print(f"3. Steam Staging & Downloads:   {report['steam_staging_downloads']['total_str']}")
print(f"4. Steam Workshop Content:      {report['steam_workshop']['total_str']}")
print(f"5. Game Crash Dumps & Logs:     {report['crash_dumps_logs']['total_str']}")
print(f"6. Save Games & Autosaves:      {report['save_games']['total_str']}")
print(f"7. Launcher Web/CEF Caches:     {report['launcher_cef_caches']['total_str']}")
print(f"8. Mod Managers Staging:        {report['mod_managers']['total_str']}")
print(f"9. Video Assets in Games:       {report['game_video_assets']['total_str']}")
print("==================================================")
