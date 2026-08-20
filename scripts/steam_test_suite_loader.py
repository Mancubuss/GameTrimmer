"""
Steam Test Games Loader & Automator for GameTrimmer Testing
============================================================
This script provides a comprehensive database of free Steam games, demos, prologues,
and benchmarks with heavy assets (redistributables, multi-language voice packs,
Bink/MP4/WebM videos, crash dumps, shader caches) across various engines
(Unreal Engine 3/4/5, Source/Source 2, Unity, Dagor, RE Engine, HeroEngine, Tiger, CryEngine, etc.).

Features:
- Export browser console JavaScript snippet to register all licenses at once on store.steampowered.com
- Generate Steam protocol installation commands (steam://install/<id>)
- Generate SteamCMD batch download script for automated/headless downloading
- Generate lightweight mock/synthetic game folders with realistic file structures for instant offline testing
- Filter by category, engine, or minimum size
"""

import json
import os
import sys
import argparse

STEAM_TEST_GAMES = [
    # --- AAA Blockbusters & Hero Shooters ---
    {
        "id": 1172470,
        "package_id": 481512,
        "name": "Apex Legends",
        "engine": "Source (Modified)",
        "approx_size_gb": 65,
        "trim_targets": ["Multi-language VPK voice packs (DE/FR/IT/ES/JP/RU/PL)", "Redistributables", "Crash logs", "Intro videos"],
        "category": "F2P Shooter"
    },
    {
        "id": 1085660,
        "package_id": 361730,
        "name": "Destiny 2",
        "engine": "Tiger Engine",
        "approx_size_gb": 105,
        "trim_targets": ["Massive localized audio packages (.pkg)", "Crash logs", "DirectX/VC redist"],
        "category": "F2P Shooter"
    },
    {
        "id": 2357570,
        "package_id": 864508,
        "name": "Overwatch 2",
        "engine": "Tank Engine (Blizzard)",
        "approx_size_gb": 50,
        "trim_targets": ["Multi-language audio voiceovers (14 languages)", "Crash handlers", "DirectX/VC redist"],
        "category": "F2P Shooter"
    },
    {
        "id": 2767030,
        "package_id": 1004128,
        "name": "Marvel Rivals",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 40,
        "trim_targets": ["UE5 CrashReportClient", "Shader caches", "Language voice packs", "Intro movies"],
        "category": "F2P Shooter"
    },
    {
        "id": 578080,
        "package_id": 182705,
        "name": "PUBG: BATTLEGROUNDS",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 35,
        "trim_targets": ["UE4 CrashReportClient", "Movies/Intro videos", "Localization assets", "DirectX/VC Redist"],
        "category": "F2P Shooter"
    },
    {
        "id": 2073850,
        "package_id": 760455,
        "name": "THE FINALS",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 20,
        "trim_targets": ["UE5 CrashReportClient", "Shader caches", "Movies", "Multi-language assets"],
        "category": "F2P Shooter"
    },
    {
        "id": 1240440,
        "package_id": 454341,
        "name": "Halo Infinite",
        "engine": "Slipspace Engine",
        "approx_size_gb": 35,
        "trim_targets": ["Language voiceover packs", "Intro/Transition MP4/Bink videos", "Redistributables"],
        "category": "F2P Shooter"
    },
    {
        "id": 2074920,
        "package_id": 760920,
        "name": "The First Descendant",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 45,
        "trim_targets": ["UE5 CrashReportClient", "Shader pipeline caches", "Multi-language pak files", "Intro movies"],
        "category": "F2P Shooter"
    },
    {
        "id": 2507950,
        "package_id": 919318,
        "name": "Delta Force",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 50,
        "trim_targets": ["UE5 CrashReportClient", "Localized audio banks", "Video cutscenes", "Anti-cheat logs"],
        "category": "F2P Shooter"
    },
    {
        "id": 2087030,
        "package_id": 765406,
        "name": "Shatterline",
        "engine": "CryEngine / Lumberyard",
        "approx_size_gb": 25,
        "trim_targets": ["CryEngine crash handler", "Video cutscenes", "Language packs", "DirectX redist"],
        "category": "F2P Shooter"
    },
    {
        "id": 677620,
        "package_id": 200844,
        "name": "Splitgate",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 20,
        "trim_targets": ["UE4 Movies", "Localization files", "Crash reporter"],
        "category": "F2P Shooter"
    },
    {
        "id": 760160,
        "package_id": 231454,
        "name": "Vampire: The Masquerade - Bloodhunt",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 30,
        "trim_targets": ["CrashReportClient", "Intro movies", "Multi-language audio", "Redist"],
        "category": "F2P Shooter"
    },

    # --- Valve Classics & Source Engines ---
    {
        "id": 730,
        "package_id": 0,
        "name": "Counter-Strike 2",
        "engine": "Source 2",
        "approx_size_gb": 35,
        "trim_targets": ["Source 2 video intros", "Sound caches", "Shader cache", "Localization text"],
        "category": "Valve / Source"
    },
    {
        "id": 570,
        "package_id": 0,
        "name": "Dota 2",
        "engine": "Source 2",
        "approx_size_gb": 45,
        "trim_targets": ["Commentary audio packs", "Panorama video backgrounds", "Redistributables"],
        "category": "Valve / Source"
    },
    {
        "id": 440,
        "package_id": 0,
        "name": "Team Fortress 2",
        "engine": "Source",
        "approx_size_gb": 22,
        "trim_targets": ["_CommonRedist (DirectX Cab files, VCRedist)", "Source sound cache", "Demos"],
        "category": "Valve / Source"
    },

    # --- MMOs & Action RPGs ---
    {
        "id": 238960,
        "package_id": 28620,
        "name": "Path of Exile",
        "engine": "Proprietary",
        "approx_size_gb": 40,
        "trim_targets": ["Massive Content.ggpk", "Mini-dumps / logs", "Multi-language resources"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 230410,
        "package_id": 26909,
        "name": "Warframe",
        "engine": "Evolution Engine",
        "approx_size_gb": 40,
        "trim_targets": ["Localization packages", "Cache files", "DirectX / Visual C++ redistributables"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 1599340,
        "package_id": 574544,
        "name": "Lost Ark",
        "engine": "Unreal Engine 3",
        "approx_size_gb": 75,
        "trim_targets": ["Massive localized audio in CookedPC", "EAC logs", "Crash logs", "DirectX redist"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 2429640,
        "package_id": 890861,
        "name": "THRONE AND LIBERTY",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 60,
        "trim_targets": ["UE4 Movies/Logos", "Shader pipeline cache", "Multi-language audio", "Crash reporter"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 2139460,
        "package_id": 785640,
        "name": "Once Human",
        "engine": "NeoX Engine",
        "approx_size_gb": 55,
        "trim_targets": ["Localization packages", "Intro video cutscenes", "Redistributables", "Shader cache"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 1286830,
        "package_id": 472535,
        "name": "STAR WARS: The Old Republic",
        "engine": "HeroEngine",
        "approx_size_gb": 50,
        "trim_targets": ["Gigabytes of French/German voiceover assets", "DirectX legacy redist", "Launcher installers"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 1985790,
        "package_id": 727781,
        "name": "Guild Wars 2",
        "engine": "Proprietary",
        "approx_size_gb": 70,
        "trim_targets": ["Massive Gw2.dat localized archives", "Crash logs", "DirectX redist"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 8500,
        "package_id": 1546,
        "name": "EVE Online",
        "engine": "Carbon Engine",
        "approx_size_gb": 30,
        "trim_targets": ["Shared cache", "DirectX redistributables", "Multi-language video/audio"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 761890,
        "package_id": 232049,
        "name": "Albion Online",
        "engine": "Unity",
        "approx_size_gb": 15,
        "trim_targets": ["Unity CrashHandler", "Multi-language asset bundles", "Unity logs"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 2064650,
        "package_id": 757134,
        "name": "Tower of Fantasy",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 35,
        "trim_targets": ["Massive JP/EN/KR localized voice packs", "Bink video cutscenes", "CrashReportClient"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 9900,
        "package_id": 2529,
        "name": "Star Trek Online",
        "engine": "Cryptic Engine",
        "approx_size_gb": 25,
        "trim_targets": ["Multi-language packs (DE/FR)", "DirectX redist", "Launcher installer"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 109600,
        "package_id": 29519,
        "name": "Neverwinter",
        "engine": "Cryptic Engine",
        "approx_size_gb": 25,
        "trim_targets": ["Localized resource hogs", "DirectX installers", "Game logs"],
        "category": "F2P MMO / RPG"
    },
    {
        "id": 24200,
        "package_id": 7525,
        "name": "DC Universe Online",
        "engine": "Unreal Engine 3",
        "approx_size_gb": 35,
        "trim_targets": ["UE3 CookedPC localization", "DirectX/VC redist", "Bink logos"],
        "category": "F2P MMO / RPG"
    },

    # --- Vehicle & Military Combat ---
    {
        "id": 236390,
        "package_id": 28312,
        "name": "War Thunder",
        "engine": "Dagor Engine",
        "approx_size_gb": 55,
        "trim_targets": ["FMOD .bank localized voice packs", "Video tutorials/intros", "Dagor crash reporter"],
        "category": "F2P Vehicle Combat"
    },
    {
        "id": 1407200,
        "package_id": 494532,
        "name": "World of Tanks",
        "engine": "Core Engine",
        "approx_size_gb": 60,
        "trim_targets": ["Wargaming crash reporter", "Multi-language voice packs", "Video tutorials", "VC redist"],
        "category": "F2P Vehicle Combat"
    },
    {
        "id": 552990,
        "package_id": 169974,
        "name": "World of Warships",
        "engine": "BigWorld Engine",
        "approx_size_gb": 65,
        "trim_targets": ["Multi-language audio banks", "DirectX / VC redistributables", "Video intros"],
        "category": "F2P Vehicle Combat"
    },
    {
        "id": 790710,
        "package_id": 244439,
        "name": "World of Warplanes",
        "engine": "BigWorld Engine",
        "approx_size_gb": 35,
        "trim_targets": ["Localized voice packs", "DirectX redist", "Wargaming error reporter"],
        "category": "F2P Vehicle Combat"
    },
    {
        "id": 2055400,
        "package_id": 754020,
        "name": "Enlisted",
        "engine": "Dagor Engine",
        "approx_size_gb": 40,
        "trim_targets": ["FMOD sound banks", "Dagor crash handler", "Video cinematics", "DirectX redist"],
        "category": "F2P Vehicle Combat"
    },
    {
        "id": 386180,
        "package_id": 78152,
        "name": "Crossout",
        "engine": "Targem Engine",
        "approx_size_gb": 15,
        "trim_targets": ["DirectX redist", "Video tutorials", "Multi-language audio"],
        "category": "F2P Vehicle Combat"
    },
    {
        "id": 212070,
        "package_id": 17294,
        "name": "Star Conflict",
        "engine": "Targem Engine",
        "approx_size_gb": 15,
        "trim_targets": ["DirectX redist", "Crash logs", "Localization files"],
        "category": "F2P Vehicle Combat"
    },

    # --- MOBAs, Battle Arenas & Unity F2P ---
    {
        "id": 444090,
        "package_id": 105073,
        "name": "Paladins",
        "engine": "Unreal Engine 3",
        "approx_size_gb": 28,
        "trim_targets": ["_CommonRedist (DirectX, VCRedist)", "CookedPC localization audio/subtitles", "Bink video logos"],
        "category": "F2P MOBA / Arena"
    },
    {
        "id": 386360,
        "package_id": 78248,
        "name": "SMITE",
        "engine": "Unreal Engine 3",
        "approx_size_gb": 30,
        "trim_targets": ["UE3 Binaries/Redist", "CookedPC language files", "Bink videos (.bik)"],
        "category": "F2P MOBA / Arena"
    },
    {
        "id": 961200,
        "package_id": 317772,
        "name": "Predecessor",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 25,
        "trim_targets": ["UE5 CrashReportClient", "Shader caches", "Multi-language audio", "Movies"],
        "category": "F2P MOBA / Arena"
    },
    {
        "id": 1203220,
        "package_id": 894178,
        "name": "NARAKA: BLADEPOINT",
        "engine": "Unity",
        "approx_size_gb": 35,
        "trim_targets": ["Unity crash handler", "Multi-language audio packs (JP/CN/EN)", "Bink/MP4 movies"],
        "category": "F2P MOBA / Arena"
    },
    {
        "id": 918450,
        "package_id": 301416,
        "name": "Century: Age of Ashes",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 15,
        "trim_targets": ["UE4 Movies", "Crash reporter", "DirectX redist"],
        "category": "F2P MOBA / Arena"
    },
    {
        "id": 304930,
        "package_id": 46162,
        "name": "Unturned",
        "engine": "Unity",
        "approx_size_gb": 5,
        "trim_targets": ["Unity crash logs", "Multi-language localization files", "Sound caches"],
        "category": "F2P Casual / Survival"
    },
    {
        "id": 291550,
        "package_id": 42168,
        "name": "Brawlhalla",
        "engine": "Proprietary",
        "approx_size_gb": 2,
        "trim_targets": ["Multi-language audio packs", "Redistributables"],
        "category": "F2P Casual / Survival"
    },

    # --- Casual, Simulation & Strategy ---
    {
        "id": 1222670,
        "package_id": 447477,
        "name": "The Sims 4",
        "engine": "SmartSim",
        "approx_size_gb": 20,
        "trim_targets": ["18 Language string/audio packs", "EA App installer", "DirectX/VC redist"],
        "category": "F2P Casual / Strategy"
    },
    {
        "id": 203770,
        "package_id": 400196,
        "name": "Crusader Kings II",
        "engine": "Clausewitz",
        "approx_size_gb": 5,
        "trim_targets": ["Multi-language packs (FR/DE/ES)", "DirectX/VC redist", "Map cache"],
        "category": "F2P Casual / Strategy"
    },
    {
        "id": 1537830,
        "package_id": 554378,
        "name": "Disney Speedstorm",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 18,
        "trim_targets": ["Multi-language voice packs", "Bink video logos", "UE4 CrashReportClient"],
        "category": "F2P Casual / Strategy"
    },
    {
        "id": 1184140,
        "package_id": 396860,
        "name": "KartRider: Drift",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 25,
        "trim_targets": ["UE4 Movies", "Multi-language sound packs", "Crash reporter"],
        "category": "F2P Casual / Strategy"
    },
    {
        "id": 2708450,
        "package_id": 981144,
        "name": "Supermarket Together",
        "engine": "Unity",
        "approx_size_gb": 4,
        "trim_targets": ["Unity crash logs", "Multi-language text files"],
        "category": "F2P Casual / Strategy"
    },
    {
        "id": 1568590,
        "package_id": 563452,
        "name": "Goose Goose Duck",
        "engine": "Unity",
        "approx_size_gb": 3,
        "trim_targets": ["Unity logs", "Multi-language assets"],
        "category": "F2P Casual / Strategy"
    },

    # --- Benchmarks & Free Tools ---
    {
        "id": 3132990,
        "package_id": 1121087,
        "name": "Black Myth: Wukong Benchmark Tool",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 8,
        "trim_targets": ["UE5 CrashReportClient", "Shader pipeline cache", "Localization packs", "Movies"],
        "category": "Free Tools & Benchmarks"
    },
    {
        "id": 231350,
        "package_id": 26615,
        "name": "3DMark Demo",
        "engine": "Proprietary",
        "approx_size_gb": 6,
        "trim_targets": ["SystemInfo installers", "DirectX cab files", "VC redistributables"],
        "category": "Free Tools & Benchmarks"
    },
    {
        "id": 365670,
        "package_id": 70808,
        "name": "Blender",
        "engine": "Open Source 3D",
        "approx_size_gb": 1.5,
        "trim_targets": ["Multi-language UI translations", "Python runtime libraries"],
        "category": "Free Tools & Benchmarks"
    },

    # --- Major AAA Free Demos & Prologues ---
    {
        "id": 2738000,
        "package_id": 994640,
        "name": "FINAL FANTASY XVI DEMO",
        "engine": "Proprietary",
        "approx_size_gb": 18,
        "trim_targets": ["Massive multi-language audio packs", "Bink video cutscenes", "DirectX redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2679460,
        "package_id": 971360,
        "name": "Metaphor: ReFantazio Prologue Demo",
        "engine": "GFD Engine (Atlus)",
        "approx_size_gb": 30,
        "trim_targets": ["Multi-language audio voiceovers", "Bink/MP4 movies", "Crash logs"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2154900,
        "package_id": 789940,
        "name": "Street Fighter 6 Demo",
        "engine": "RE Engine",
        "approx_size_gb": 16,
        "trim_targets": ["RE Engine crash handler", "Multi-language voice packs", "Shader cache", "DirectX redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2243880,
        "package_id": 804791,
        "name": "Resident Evil 4 Chainsaw Demo",
        "engine": "RE Engine",
        "approx_size_gb": 11,
        "trim_targets": ["RE Engine localized audio", "DirectX/VC redistributables", "Crash dump handler"],
        "category": "Major AAA Demo"
    },
    {
        "id": 1105510,
        "package_id": 371727,
        "name": "Detroit: Become Human Demo",
        "engine": "Quantic Dream Engine",
        "approx_size_gb": 10,
        "trim_targets": ["Massive localized voiceover packages", "Intro video logos", "Vulkan/DirectX redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2515080,
        "package_id": 922110,
        "name": "RoboCop: Rogue City Demo",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 25,
        "trim_targets": ["UE5 CrashReportClient", "Video cutscenes", "Multi-language assets", "Shader cache"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2384010,
        "package_id": 873830,
        "name": "Lies of P Demo",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 20,
        "trim_targets": ["UE4 Movies", "Localized audio packs", "Crash reporter", "DirectX redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2518420,
        "package_id": 923230,
        "name": "Ghostrunner 2 Demo",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 12,
        "trim_targets": ["UE5 CrashReportClient", "Shader pipeline cache", "Movies", "Multi-language"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2577610,
        "package_id": 941420,
        "name": "The Talos Principle 2 Demo",
        "engine": "Unreal Engine 5",
        "approx_size_gb": 15,
        "trim_targets": ["UE5 movies", "Shader caches", "Localization text/audio", "Crash reporter"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2769490,
        "package_id": 1004820,
        "name": "Pacific Drive Demo",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 14,
        "trim_targets": ["UE4 CrashReportClient", "Bink video cutscenes", "Localization files"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2898490,
        "package_id": 1039860,
        "name": "Visions of Mana Demo",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 15,
        "trim_targets": ["UE4 movies", "Multi-language voice packs (JP/EN)", "DirectX redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 2908860,
        "package_id": 1043250,
        "name": "Kunitsu-Gami: Path of the Goddess Demo",
        "engine": "RE Engine",
        "approx_size_gb": 8,
        "trim_targets": ["RE Engine crash reporter", "Shader cache", "Voice packs", "DirectX redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 1433600,
        "package_id": 504824,
        "name": "OUTRIDERS Demo",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 22,
        "trim_targets": ["UE4 Movies", "Multi-language voice packs (7 languages)", "DirectX/VC redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 1354890,
        "package_id": 470762,
        "name": "DRAGON QUEST XI S Demo",
        "engine": "Unreal Engine 4",
        "approx_size_gb": 15,
        "trim_targets": ["UE4 sound archives", "Multi-language voice packs", "DirectX redist"],
        "category": "Major AAA Demo"
    },
    {
        "id": 319630,
        "package_id": 50669,
        "name": "Life is Strange (Episode 1)",
        "engine": "Unreal Engine 3",
        "approx_size_gb": 3,
        "trim_targets": ["_CommonRedist", "CookedPC language files", "Bink video (.bik)"],
        "category": "Major AAA Demo"
    }
]

def filter_games(games, category=None, engine=None, min_size=None):
    filtered = games
    if category:
        cat_lower = category.lower()
        filtered = [g for g in filtered if cat_lower in g["category"].lower()]
    if engine:
        eng_lower = engine.lower()
        filtered = [g for g in filtered if eng_lower in g["engine"].lower()]
    if min_size is not None:
        filtered = [g for g in filtered if g["approx_size_gb"] >= min_size]
    return filtered

def generate_browser_js(games_list):
    sub_ids = [g["package_id"] for g in games_list if g["package_id"] > 0]
    app_ids = [g["id"] for g in games_list]
    
    js = f"""// === SteamDB / Steam Free Game License Activator ===
// Open https://store.steampowered.com/account/licenses/ in your browser
// Press F12 -> Console -> Paste and run this script:

(async function addFreeGames() {{
    const packages = {json.dumps(sub_ids)};
    const apps = {json.dumps(app_ids)};
    console.log(`[GameTrimmer Test Setup] Starting activation of ${{packages.length}} packages...`);
    
    let successCount = 0;
    let failedCount = 0;
    for (let i = 0; i < packages.length; i++) {{
        const subId = packages[i];
        try {{
            const res = await fetch('/checkout/addfreelicense', {{
                method: 'POST',
                headers: {{ 'Content-Type': 'application/x-www-form-urlencoded; charset=UTF-8' }},
                body: new URLSearchParams({{
                    action: 'add_to_cart',
                    sessionid: g_sessionID,
                    subid: subId
                }})
            }});
            console.log(`[+] [${{i+1}}/${{packages.length}}] Added package SubID: ${{subId}}`);
            successCount++;
            await new Promise(r => setTimeout(r, 450)); // anti-rate-limit delay
        }} catch (err) {{
            console.warn(`[-] Failed SubID ${{subId}}:`, err);
            failedCount++;
        }}
    }}
    console.log(`[GameTrimmer Test Setup] Done! Successfully registered ${{successCount}} packages (Failed: ${{failedCount}}).`);
    console.log(`To install games, use steam://install/<AppID> or launch the Steam Client.`);
}})();
"""
    return js

def generate_steam_protocol_bat(games_list):
    lines = [
        "@echo off",
        "echo ========================================================",
        "echo  GameTrimmer: Queuing Steam Game Installations",
        "echo ========================================================",
        ""
    ]
    for g in games_list:
        lines.append(f'echo Installing {g["name"]} ({g["engine"]} - {g["approx_size_gb"]} GB)...')
        lines.append(f'start "" "steam://install/{g["id"]}"')
        lines.append('timeout /t 2 /nobreak >nul')
    lines.append("")
    lines.append("echo [DONE] All installation requests sent to Steam Client.")
    return "\n".join(lines)

def generate_steamcmd_script(games_list):
    lines = [
        "// SteamCMD script for downloading GameTrimmer test suite",
        "// Run with: steamcmd +runscript steamcmd_download_suite.txt",
        "login <YOUR_STEAM_USERNAME>",
        "force_install_dir ./steam_test_games/",
    ]
    for g in games_list:
        lines.append(f"// {g['name']} ({g['engine']} - {g['approx_size_gb']} GB)")
        lines.append(f"app_update {g['id']} validate")
    lines.append("quit")
    return "\n".join(lines)

def create_mock_game_library(dest_dir: str):
    os.makedirs(dest_dir, exist_ok=True)
    mock_games = [
        {
            "folder": "Apex Legends",
            "files": [
                ("r5apex.exe", 1024 * 1024),
                ("audio/ship/general_german.vpk", 500 * 1024 * 1024),
                ("audio/ship/general_french.vpk", 480 * 1024 * 1024),
                ("audio/ship/general_japanese.vpk", 520 * 1024 * 1024),
                ("audio/ship/general_english.vpk", 510 * 1024 * 1024),
                ("media/intro.bik", 85 * 1024 * 1024),
                ("media/respawn_logo.bik", 35 * 1024 * 1024),
                ("_CommonRedist/DirectX/Jun2010/Apr2005_d3dx9_24_x64.cab", 25 * 1024 * 1024),
                ("_CommonRedist/DirectX/Jun2010/dxsetup.exe", 15 * 1024 * 1024),
                ("_CommonRedist/vcredist/2019/vc_redist.x64.exe", 24 * 1024 * 1024),
                ("crashdumps/crash_2026_01.dmp", 12 * 1024 * 1024)
            ]
        },
        {
            "folder": "Paladins",
            "files": [
                ("Binaries/Win64/Paladins.exe", 1024 * 1024),
                ("Binaries/Redist/InstallHirezService.exe", 45 * 1024 * 1024),
                ("ChaosGame/CookedPC/Localization/DEU/Audio_DEU.upk", 350 * 1024 * 1024),
                ("ChaosGame/CookedPC/Localization/FRA/Audio_FRA.upk", 340 * 1024 * 1024),
                ("ChaosGame/CookedPC/Localization/ESN/Audio_ESN.upk", 330 * 1024 * 1024),
                ("ChaosGame/CookedPC/Localization/RUS/Audio_RUS.upk", 360 * 1024 * 1024),
                ("ChaosGame/Movies/Logo_EvilMojo.bik", 20 * 1024 * 1024),
                ("ChaosGame/Movies/HiRezLogo.bik", 18 * 1024 * 1024),
                ("Engine/Config/CrashReportClient.exe", 14 * 1024 * 1024)
            ]
        },
        {
            "folder": "The Sims 4",
            "files": [
                ("Game/Bin/TS4_x64.exe", 1024 * 1024),
                ("Data/Client/Strings_RUS_RU.package", 120 * 1024 * 1024),
                ("Data/Client/Strings_GER_DE.package", 115 * 1024 * 1024),
                ("Data/Client/Strings_FRE_FR.package", 118 * 1024 * 1024),
                ("Data/Client/Strings_ITA_IT.package", 110 * 1024 * 1024),
                ("Data/Client/Strings_JPN_JP.package", 125 * 1024 * 1024),
                ("__Installer/EAappInstaller.exe", 80 * 1024 * 1024),
                ("__Installer/vc/vc2015/redist/vc_redist.x64.exe", 20 * 1024 * 1024)
            ]
        },
        {
            "folder": "Black Myth Wukong Benchmark",
            "files": [
                ("b1.exe", 1024 * 1024),
                ("b1/Binaries/Win64/CrashReportClient-Win64-Shipping.exe", 25 * 1024 * 1024),
                ("b1/Content/Paks/b1-Audio-German.pak", 400 * 1024 * 1024),
                ("b1/Content/Paks/b1-Audio-French.pak", 390 * 1024 * 1024),
                ("b1/Content/Movies/GameScienceLogo.mp4", 55 * 1024 * 1024),
                ("b1/Saved/ShaderPipelineCache/PCD3D_SM6.upipelinecache", 85 * 1024 * 1024)
            ]
        },
        {
            "folder": "Destiny 2",
            "files": [
                ("destiny2.exe", 1024 * 1024),
                ("packages/audio_german_01.pkg", 600 * 1024 * 1024),
                ("packages/audio_french_01.pkg", 580 * 1024 * 1024),
                ("packages/audio_japanese_01.pkg", 610 * 1024 * 1024),
                ("packages/audio_russian_01.pkg", 590 * 1024 * 1024),
                ("temp/crash_report_2026.dmp", 30 * 1024 * 1024)
            ]
        },
        {
            "folder": "War Thunder",
            "files": [
                ("launcher.exe", 1024 * 1024),
                ("sound/voice_de.bank", 320 * 1024 * 1024),
                ("sound/voice_fr.bank", 310 * 1024 * 1024),
                ("sound/voice_it.bank", 290 * 1024 * 1024),
                ("video/tutorial_carrier.bik", 75 * 1024 * 1024),
                ("bpreport.exe", 15 * 1024 * 1024)
            ]
        }
    ]
    
    print(f"[*] Creating synthetic game library in: {dest_dir}")
    total_mock_bytes = 0
    for game in mock_games:
        game_path = os.path.join(dest_dir, game["folder"])
        for rel_file, size in game["files"]:
            full_file_path = os.path.join(game_path, rel_file)
            os.makedirs(os.path.dirname(full_file_path), exist_ok=True)
            with open(full_file_path, "wb") as f:
                if size > 0:
                    f.seek(size - 1)
                    f.write(b"\0")
            total_mock_bytes += size
            
    print(f"[+] Successfully generated {len(mock_games)} mock games ({total_mock_bytes / (1024*1024*1024):.2f} GB virtual size) for fast GameTrimmer validation!")

def main():
    parser = argparse.ArgumentParser(description="Steam Test Games Suite for GameTrimmer")
    parser.add_argument("--save-js", type=str, default="scripts/activate_steam_test_licenses.js", help="Path to save the browser JS script")
    parser.add_argument("--save-bat", type=str, default="scripts/install_steam_test_games.bat", help="Path to save the Steam URI batch script")
    parser.add_argument("--save-steamcmd", type=str, default="scripts/steamcmd_download_suite.txt", help="Path to save the SteamCMD script")
    parser.add_argument("--create-mock-library", type=str, help="Generate a mock Steam library folder with realistic dummy target files")
    parser.add_argument("--list", action="store_true", help="List all curated games and their GameTrimmer trim targets")
    parser.add_argument("--category", type=str, help="Filter games by category substring")
    parser.add_argument("--engine", type=str, help="Filter games by engine substring")
    parser.add_argument("--min-size", type=float, help="Filter games by minimum approximate size in GB")
    
    args = parser.parse_args()
    
    selected_games = filter_games(STEAM_TEST_GAMES, category=args.category, engine=args.engine, min_size=args.min_size)
    
    if args.list or len(sys.argv) == 1:
        total_gb = sum(g["approx_size_gb"] for g in selected_games)
        print("=" * 100)
        print(f" CURATED STEAM FREE GAMES & DEMOS FOR GAMETRIMMER TESTING (Count: {len(selected_games)}, Total Size: ~{total_gb:.0f} GB)")
        print("=" * 100)
        for g in selected_games:
            print(f"• {g['name']:<38} | AppID: {g['id']:<8} | ~{g['approx_size_gb']:>3} GB | [{g['category']}] ({g['engine']})")
            print(f"  Targets: {', '.join(g['trim_targets'])}")
            print("-" * 100)

    # Save artifacts
    js_content = generate_browser_js(selected_games)
    if args.save_js:
        with open(args.save_js, "w", encoding="utf-8") as f:
            f.write(js_content)
        print(f"[+] Saved browser license activation script ({len(selected_games)} games) to: {args.save_js}")
        
    bat_content = generate_steam_protocol_bat(selected_games)
    if args.save_bat:
        with open(args.save_bat, "w", encoding="utf-8") as f:
            f.write(bat_content)
        print(f"[+] Saved Steam desktop client installer script to: {args.save_bat}")
        
    steamcmd_content = generate_steamcmd_script(selected_games)
    if args.save_steamcmd:
        with open(args.save_steamcmd, "w", encoding="utf-8") as f:
            f.write(steamcmd_content)
        print(f"[+] Saved SteamCMD automated script to: {args.save_steamcmd}")
        
    if args.create_mock_library:
        create_mock_game_library(args.create_mock_library)

if __name__ == "__main__":
    main()
