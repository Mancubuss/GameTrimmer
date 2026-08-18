import sqlite3
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

for p in [r"dist\GameTrimmer-1.0.0\gametrimmer.db", r"gametrimmer.db"]:
    if os.path.exists(p):
        print(f"=== Opening DB at: {p} ===")
        conn = sqlite3.connect(p)
        cur = conn.cursor()
        targets = ["Far Cry", "CAYNE", "Prey", "Alan Wake's American Nightmare", "SOMA"]

        for name in targets:
            print(f"\n==================== Game: {name} ====================")
            cur.execute("""
                SELECT g.id, gl.vendor, gl.path as lib_path, g.install_dir, g.app_id,
                       (SELECT COUNT(*) FROM files f WHERE f.game_id = g.id) as total_files,
                       (SELECT COUNT(*) FROM findings fi JOIN files f ON f.id = fi.file_id WHERE f.game_id = g.id) as finding_count
                FROM games g
                JOIN game_libraries gl ON gl.id = g.library_id
                WHERE g.name = ?
            """, (name,))
            for row in cur.fetchall():
                print(f"  [Game ID {row[0]}] Launcher: {row[1].upper():<7} | Lib: {row[2]} | InstallDir: {row[3]} | AppID: {row[4]} | Findings: {row[6]}")
