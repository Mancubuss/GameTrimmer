import sqlite3
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

candidates = [
    os.path.expandvars(r"%LOCALAPPDATA%\GameTrimmer\gametrimmer.db"),
    os.path.expandvars(r"%APPDATA%\GameTrimmer\gametrimmer.db"),
    r"dist\GameTrimmer-1.0.0\gametrimmer.db",
    r"gametrimmer.db"
]

found = False
for p in candidates:
    if os.path.exists(p):
        print(f"=== Found DB at: {p} ===")
        found = True
        conn = sqlite3.connect(p)
        cur = conn.cursor()
        
        cur.execute("SELECT id, vendor, path FROM game_libraries ORDER BY id")
        print("\n--- Game Libraries ---")
        libs = {}
        for r in cur.fetchall():
            libs[r[0]] = (r[1], r[2])
            print(f"Lib ID {r[0]}: Vendor={r[1]}, Path={r[2]}")
            
        cur.execute("SELECT id, library_id, name, install_dir, app_id FROM games ORDER BY name, id")
        games = cur.fetchall()
        print(f"\n--- Total Games: {len(games)} ---")
        
        # Check duplicate names
        from collections import defaultdict
        name_map = defaultdict(list)
        for g in games:
            name_map[g[2]].append(g)
            
        duplicates = {k: v for k, v in name_map.items() if len(v) > 1}
        print(f"\n--- Duplicate Game Names ({len(duplicates)} names with multiple entries) ---")
        for name, glist in duplicates.items():
            print(f"\nGame: «{name}» ({len(glist)} entries):")
            for g in glist:
                lib_info = libs.get(g[1], ("unknown", "unknown"))
                print(f"  Game ID={g[0]}, LibID={g[1]} ({lib_info[0]} @ {lib_info[1]}), InstallDir={g[3]}, AppID={g[4]}")
                
        cur.execute("SELECT COUNT(*), COUNT(DISTINCT file_id) FROM findings")
        fcount = cur.fetchone()
        print(f"\n--- Findings: {fcount[0]} total rows, {fcount[1]} distinct file_ids ---")
        
if not found:
    print("No database file found in candidate locations.")
