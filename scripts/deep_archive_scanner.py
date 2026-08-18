import os
import sys
import json
import struct
import time
import zipfile
import re
import subprocess

REPAK_PATH = r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\archive_tools\bin\repak.exe"

NON_ENGLISH_LANG_TAGS = {
    'fr', 'fra', 'french', 'fr-fr', 'fr-ca',
    'de', 'deu', 'german', 'de-de',
    'es', 'spa', 'spanish', 'es-es', 'es-mx',
    'it', 'ita', 'italian', 'it-it',
    'ru', 'rus', 'russian', 'ru-ru',
    'ja', 'jpn', 'japanese', 'ja-jp',
    'zh', 'zho', 'chi', 'chinese', 'zh-cn', 'zh-tw', 'chs', 'cht', 'simplifiedchinese', 'traditionalchinese',
    'ko', 'kor', 'korean', 'ko-kr',
    'pt', 'por', 'portuguese', 'pt-br', 'pt-pt', 'brazilian',
    'pl', 'pol', 'polish', 'pl-pl',
    'tr', 'tur', 'turkish', 'tr-tr',
    'ar', 'ara', 'arabic',
    'cz', 'ces', 'czech', 'cs',
    'hu', 'hun', 'hungarian',
    'nl', 'nld', 'dutch',
    'th', 'tha', 'thai',
    'vi', 'vie', 'vietnamese',
    'uk', 'ukr', 'ukrainian'
}

def is_non_english_name(name):
    lower = name.lower()
    for tag in NON_ENGLISH_LANG_TAGS:
        pattern = r'(^|[_\-./\\ ])' + re.escape(tag) + r'([_\-./\\ ]|$)'
        if re.search(pattern, lower):
            return True, tag
    return False, None

def parse_wwise_pck(file_path):
    """Parses Wwise AKPK header and language table."""
    try:
        size = os.path.getsize(file_path)
        if size < 28:
            return None
        with open(file_path, 'rb') as f:
            magic = f.read(4)
            if magic != b'AKPK':
                return None
            hdr_size = struct.unpack('<I', f.read(4))[0]
            if size < 8 + hdr_size:
                return None
            hdr_data = f.read(hdr_size)
            if len(hdr_data) < 20:
                return None
            v, l_size, b_size, s_size, e_size = struct.unpack('<5I', hdr_data[:20])
            if l_size < 4:
                return None
            
            lang_map_start = 20
            lang_count = struct.unpack('<I', hdr_data[lang_map_start:lang_map_start+4])[0]
            pos = lang_map_start + 4
            
            languages = {}
            for _ in range(lang_count):
                if pos + 8 > lang_map_start + l_size:
                    break
                str_off, lid = struct.unpack('<II', hdr_data[pos:pos+8])
                pos += 8
                s_pos = lang_map_start + str_off
                end = hdr_data.find(b'\x00\x00', s_pos)
                if end == -1 or end > lang_map_start + l_size:
                    end = lang_map_start + l_size
                elif (end - s_pos) % 2 != 0:
                    end += 1
                name = hdr_data[s_pos:end].decode('utf-16le', errors='replace').strip()
                languages[lid] = name

            non_en_langs = [name for lid, name in languages.items() if lid != 0 and 'english' not in name.lower() and 'sfx' not in name.lower()]

            if non_en_langs:
                total_langs = max(1, len(languages))
                non_en_ratio = len(non_en_langs) / total_langs
                savings = int(size * non_en_ratio * 0.95)
                return {
                    'type': 'Wwise PCK Monolith',
                    'size': size,
                    'languages': list(languages.values()),
                    'non_en_langs': non_en_langs,
                    'estimated_savings': savings,
                    'method': 'Wwise Direct Trim / In-place Zeroing',
                    'loose_override': False,
                    'repack_needed': False
                }
    except Exception:
        pass
    return None

def parse_zip_cryengine_pak(file_path):
    """Inspects standard Zip/CryEngine/Godot/Love2D .pak or .zip archives for embedded localization."""
    try:
        size = os.path.getsize(file_path)
        if size < 100 or size > 15 * 1024 * 1024 * 1024:
            return None
        if not zipfile.is_zipfile(file_path):
            return None
        with zipfile.ZipFile(file_path, 'r') as z:
            total_uncomp = 0
            non_en_uncomp = 0
            non_en_files = []
            file_count = 0
            for info in z.infolist():
                file_count += 1
                total_uncomp += info.file_size
                is_non_en, tag = is_non_english_name(info.filename)
                if is_non_en and any(info.filename.lower().endswith(ext) for ext in ['.wav', '.ogg', '.mp3', '.bik', '.bk2', '.wem', '.bnk', '.xml', '.locres', '.txt', '.csv']):
                    non_en_uncomp += info.file_size
                    if len(non_en_files) < 10:
                        non_en_files.append(info.filename)
            
            if non_en_uncomp > 5 * 1024 * 1024:
                ratio = non_en_uncomp / max(1, total_uncomp)
                savings = int(size * ratio)
                return {
                    'type': 'Zip/CryEngine PAK',
                    'size': size,
                    'file_count': file_count,
                    'non_en_files_sample': non_en_files,
                    'non_en_uncompressed': non_en_uncomp,
                    'estimated_savings': savings,
                    'method': 'Zip Unpack & Loose Files Override / Repack',
                    'loose_override': True,
                    'repack_needed': False
                }
    except Exception:
        pass
    return None

def inspect_unreal_pak(file_path):
    """Uses repak list to inspect files inside Unreal Engine PAK files."""
    if not os.path.exists(REPAK_PATH):
        return None
    try:
        size = os.path.getsize(file_path)
        if size < 50 * 1024 * 1024 or size > 25 * 1024 * 1024 * 1024: # between 50MB and 25GB
            return None
        cmd = [REPAK_PATH, "list", file_path]
        res = subprocess.run(cmd, capture_output=True, text=True, timeout=12)
        if res.returncode == 0 and res.stdout:
            lines = res.stdout.strip().splitlines()
            if not lines:
                return None
            non_en_files = []
            for l in lines:
                is_non_en, tag = is_non_english_name(l)
                if is_non_en and any(x in l.lower() for x in ['audio', 'voice', 'loc', 'movie', 'dialog', 'wwise', 'wem', 'uasset', 'locres']):
                    non_en_files.append(l)
            if len(non_en_files) > 10:
                ratio = min(0.40, len(non_en_files) / max(1, len(lines)))
                savings = int(size * ratio)
                if savings > 10 * 1024 * 1024:
                    return {
                        'type': 'Unreal Engine Monolithic PAK',
                        'size': size,
                        'total_files': len(lines),
                        'non_en_files_count': len(non_en_files),
                        'non_en_sample': non_en_files[:5],
                        'estimated_savings': savings,
                        'method': 'repak Unpack -> Delete Lang -> repak Pack / Loose Content',
                        'loose_override': True,
                        'repack_needed': False
                    }
    except Exception:
        pass
    return None

def parse_electron_asar(file_path):
    """Inspects Electron app.asar for multi-language packages."""
    try:
        size = os.path.getsize(file_path)
        if size < 1024:
            return None
        with open(file_path, 'rb') as f:
            header_size_bytes = f.read(16)
            if len(header_size_bytes) < 16:
                return None
            u1, u2, u3, json_len = struct.unpack('<4I', header_size_bytes)
            if u1 == 4 and u2 == json_len + 8:
                return {
                    'type': 'Electron ASAR',
                    'size': size,
                    'method': 'Unpack to Loose Folder (asar extract) - No Repack Needed',
                    'loose_override': True,
                    'repack_needed': False,
                    'estimated_savings': int(size * 0.15)
                }
    except Exception:
        pass
    return None

def analyze_game_directory(game_path, game_name="", max_depth=6):
    results = {
        'game_name': game_name,
        'game_path': game_path,
        'total_game_size': 0,
        'archives_found': [],
        'engines_detected': set(),
        'total_estimated_savings': 0,
        'methods_available': set()
    }
    
    total_size = 0
    file_list = []
    
    try:
        for root, dirs, files in os.walk(game_path):
            rel_root = os.path.relpath(root, game_path)
            depth = 0 if rel_root == '.' else rel_root.count(os.sep) + 1
            if depth > max_depth:
                dirs.clear()
                continue
            
            for file in files:
                full_path = os.path.join(root, file)
                try:
                    sz = os.path.getsize(full_path)
                    total_size += sz
                    ext = os.path.splitext(file)[1].lower()
                    file_list.append((full_path, rel_root, file, ext, sz))
                except Exception:
                    continue
    except Exception as e:
        results['error'] = str(e)
        return results

    results['total_game_size'] = total_size
    
    # 1. Wwise PCKs
    pck_files = [f for f in file_list if f[3] == '.pck']
    for full_path, rel_root, file, ext, sz in pck_files:
        pck_info = parse_wwise_pck(full_path)
        if pck_info and pck_info['estimated_savings'] > 1024 * 1024:
            results['archives_found'].append({
                'file': os.path.join(rel_root, file),
                'full_path': full_path,
                'info': pck_info
            })
            results['total_estimated_savings'] += pck_info['estimated_savings']
            results['engines_detected'].add('Wwise Audio Engine')
            results['methods_available'].add('Wwise Audio Direct Trim / In-place Zeroing')

    # 2. Capcom RE Engine PAKs
    re_paks = [f for f in file_list if 're_chunk' in f[2].lower() or (f[3] == '.pak' and any(k in game_name.lower() for k in ['resident', 'devil may cry', 'monster hunter', 'dragons dogma']))]
    for full_path, rel_root, file, ext, sz in re_paks:
        if sz > 1024 * 1024 * 1024:
            savings = int(sz * 0.28)
            results['archives_found'].append({
                'file': os.path.join(rel_root, file),
                'full_path': full_path,
                'info': {
                    'type': 'Capcom RE Engine PAK',
                    'size': sz,
                    'estimated_savings': savings,
                    'method': 'REE.Unpacker -> Strip Wwise Voice -> REE.Packer / Loose Files (Fluffy Mod Manager)',
                    'loose_override': True,
                    'repack_needed': False
                }
            })
            results['total_estimated_savings'] += savings
            results['engines_detected'].add('Capcom RE Engine')
            results['methods_available'].add('RE Engine Repack / Loose Mod Framework')

    # 3. Unreal Engine 4/5 PAK & IoStore (.pak, .utoc/.ucas)
    ue_paks = [f for f in file_list if f[3] in ['.pak', '.utoc'] and ('content' in f[1].lower() or 'paks' in f[1].lower())]
    if ue_paks:
        results['engines_detected'].add('Unreal Engine 4/5')
        for full_path, rel_root, file, ext, sz in ue_paks:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 5 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'Unreal Engine Separate Lang PAK',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct PAK File Deletion / Disable',
                        'loose_override': False,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
            elif sz > 50 * 1024 * 1024 and ext == '.pak':
                ue_inspect = inspect_unreal_pak(full_path)
                if ue_inspect and ue_inspect['estimated_savings'] > 10 * 1024 * 1024:
                    results['archives_found'].append({
                        'file': os.path.join(rel_root, file),
                        'full_path': full_path,
                        'info': ue_inspect
                    })
                    results['total_estimated_savings'] += ue_inspect['estimated_savings']
                    results['methods_available'].add('repak Unpack / Loose Files Override / Repack')

    # 4. Bethesda Creation Engine (.ba2, .bsa)
    bethesda_archives = [f for f in file_list if f[3] in ['.ba2', '.bsa']]
    if bethesda_archives:
        results['engines_detected'].add('Bethesda Creation Engine / Gamebryo')
        for full_path, rel_root, file, ext, sz in bethesda_archives:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 10 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': f'Bethesda {ext.upper()} Lang Archive',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / Loose Files Override (bInvalidateOlderFiles=1)',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('Bethesda Loose Files Priority Override')

    # 5. Valve Source / Source 2 Engine (.vpk)
    vpk_files = [f for f in file_list if f[3] == '.vpk']
    if vpk_files:
        results['engines_detected'].add('Valve Source / Source 2')
        for full_path, rel_root, file, ext, sz in vpk_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 5 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'Source VPK Lang Pack',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / Loose Files Override',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('Source Loose Files Override')

    # 6. Unity Engine (.assets, .bundle, UnityFS)
    unity_files = [f for f in file_list if f[3] in ['.assets', '.bundle', '.unity3d'] or 'sharedassets' in f[2].lower()]
    if unity_files:
        results['engines_detected'].add('Unity Engine')
        for full_path, rel_root, file, ext, sz in unity_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 5 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'Unity Localized AssetBundle',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / UnityPy Unpack & Modify',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('Unity AssetBundle Trimming')

    # 7. CryEngine / Lumberyard / Zip PAKs
    pak_files = [f for f in file_list if f[3] == '.pak' and full_path not in [a['full_path'] for a in results['archives_found']]]
    for full_path, rel_root, file, ext, sz in pak_files:
        is_non_en, tag = is_non_english_name(file)
        if is_non_en and sz > 5 * 1024 * 1024:
            results['archives_found'].append({
                'file': os.path.join(rel_root, file),
                'full_path': full_path,
                'info': {
                    'type': 'Separate Lang PAK',
                    'size': sz,
                    'estimated_savings': sz,
                    'method': 'Direct Deletion',
                    'loose_override': False,
                    'repack_needed': False
                }
            })
            results['total_estimated_savings'] += sz
        elif sz > 20 * 1024 * 1024 and sz < 5 * 1024 * 1024 * 1024:
            zip_info = parse_zip_cryengine_pak(full_path)
            if zip_info and zip_info['estimated_savings'] > 5 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': zip_info
                })
                results['total_estimated_savings'] += zip_info['estimated_savings']
                results['engines_detected'].add('CryEngine / Zip PAK Engine')
                results['methods_available'].add('Zip PAK Loose Files Override (sys_pak_priority=0)')

    # 8. Electron app.asar
    asar_files = [f for f in file_list if f[2].lower() == 'app.asar']
    for full_path, rel_root, file, ext, sz in asar_files:
        asar_info = parse_electron_asar(full_path)
        if asar_info:
            results['archives_found'].append({
                'file': os.path.join(rel_root, file),
                'full_path': full_path,
                'info': asar_info
            })
            results['total_estimated_savings'] += asar_info['estimated_savings']
            results['engines_detected'].add('Electron / HTML5')
            results['methods_available'].add('Electron Loose Files Extraction (asar extract)')

    # 9. CRIWARE CPK (.cpk)
    cpk_files = [f for f in file_list if f[3] == '.cpk']
    if cpk_files:
        results['engines_detected'].add('CRIWARE Engine')
        for full_path, rel_root, file, ext, sz in cpk_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 10 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'CRIWARE CPK Lang Package',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / CriPakTools Loose Override',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('CRIWARE CPK Loose / Mod Override')

    # 10. FMOD Sound Banks (.bank, .fsb)
    fmod_files = [f for f in file_list if f[3] in ['.bank', '.fsb']]
    if fmod_files:
        results['engines_detected'].add('FMOD Sound System')
        for full_path, rel_root, file, ext, sz in fmod_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 5 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'FMOD Localized SoundBank',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / FMOD Loose SoundBank',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('FMOD SoundBank Trim')

    # 11. Bink Videos (.bik, .bk2)
    bink_files = [f for f in file_list if f[3] in ['.bik', '.bk2']]
    if bink_files:
        non_en_bink = [f for f in bink_files if is_non_english_name(f[2])[0]]
        if non_en_bink:
            bink_sum = sum(f[4] for f in non_en_bink)
            if bink_sum > 10 * 1024 * 1024:
                results['archives_found'].append({
                    'file': f"{len(non_en_bink)} localized Bink video files",
                    'full_path': non_en_bink[0][0],
                    'info': {
                        'type': 'Bink Video Localized Cutscenes',
                        'size': bink_sum,
                        'estimated_savings': bink_sum,
                        'method': 'Direct Deletion / Null Video Stub (0 KB replacement)',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += bink_sum
                results['methods_available'].add('Bink Video Trimming / Zero Stubbing')

    # 12. CD Projekt RED Engine (.bundle, .archive)
    cdpr_files = [f for f in file_list if f[3] in ['.bundle', '.archive'] and any(x in f[1].lower() for x in ['content', 'r6', 'pc', 'bundles'])]
    if cdpr_files:
        results['engines_detected'].add('CD Projekt RED REDengine')
        for full_path, rel_root, file, ext, sz in cdpr_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 20 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'REDengine Localized Archive',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Archive Deletion / WolvenKit Loose Override',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('REDengine Loose Archive Mod Override')

    # 13. Ubisoft Anvil / Dunia (.forge, .fat/.dat)
    ubisoft_files = [f for f in file_list if f[3] in ['.forge', '.fat', '.dat'] and any(x in f[1].lower() for x in ['datapc', 'worlds', 'sound'])]
    if ubisoft_files:
        results['engines_detected'].add('Ubisoft Anvil / Dunia Engine')
        for full_path, rel_root, file, ext, sz in ubisoft_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 15 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': f'Ubisoft {ext.upper()} Lang Package',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / AnvilToolkit Loose Files',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('Ubisoft Anvil/Dunia Loose Override')

    # 14. Square Enix / Crystal Dynamics (.tiger)
    tiger_files = [f for f in file_list if f[3] == '.tiger']
    if tiger_files:
        results['engines_detected'].add('Square Enix Crystal Engine')
        for full_path, rel_root, file, ext, sz in tiger_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 20 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'Tiger Archive Lang Pack',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / TigerUnpacker Loose Files',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['methods_available'].add('Tiger Loose Override')

    # 15. Decima & Insomniac (.bin, .core, .toc/.archive)
    sony_files = [f for f in file_list if f[3] in ['.bin', '.core'] and any(x in f[1].lower() for x in ['content', 'packed_dx12', 'sound', 'audio'])]
    if sony_files:
        for full_path, rel_root, file, ext, sz in sony_files:
            is_non_en, tag = is_non_english_name(file)
            if is_non_en and sz > 30 * 1024 * 1024:
                results['archives_found'].append({
                    'file': os.path.join(rel_root, file),
                    'full_path': full_path,
                    'info': {
                        'type': 'Decima / Sony Port Lang Archive',
                        'size': sz,
                        'estimated_savings': sz,
                        'method': 'Direct Deletion / Decima Explorer Loose Files',
                        'loose_override': True,
                        'repack_needed': False
                    }
                })
                results['total_estimated_savings'] += sz
                results['engines_detected'].add('Decima Engine / Sony Port')
                results['methods_available'].add('Decima Loose Override')

    results['engines_detected'] = list(results['engines_detected'])
    results['methods_available'] = list(results['methods_available'])
    return results

def main():
    if len(sys.argv) < 3:
        print("Usage: deep_archive_scanner.py <input_games_json> <output_json> [start_index] [count]")
        sys.exit(1)
        
    input_file = sys.argv[1]
    output_file = sys.argv[2]
    start_idx = int(sys.argv[3]) if len(sys.argv) > 3 else 0
    count = int(sys.argv[4]) if len(sys.argv) > 4 else 999999

    with open(input_file, 'r', encoding='utf-8') as f:
        games = json.load(f)

    subset = games[start_idx:start_idx + count]
    print(f"Scanning {len(subset)} games (from index {start_idx} to {start_idx + len(subset) - 1})...")

    results = []
    t0 = time.time()

    for idx, g in enumerate(subset):
        path = g.get('Path')
        name = g.get('Name') or g.get('InstallDir') or os.path.basename(path)
        if not path or not os.path.exists(path):
            continue
        
        t_game = time.time()
        res = analyze_game_directory(path, name)
        if res and (res['archives_found'] or res['total_estimated_savings'] > 0):
            results.append(res)
            savings_mb = res['total_estimated_savings'] / (1024 * 1024)
            savings_gb = res['total_estimated_savings'] / (1024 * 1024 * 1024)
            engines = ", ".join(res['engines_detected']) if res['engines_detected'] else "Unknown"
            print(f"[{idx+1}/{len(subset)}] FOUND: {name} ({engines}) -> Est Savings: {savings_mb:.1f} MB ({savings_gb:.2f} GB)")

    elapsed = time.time() - t0
    total_savings_bytes = sum(r['total_estimated_savings'] for r in results)
    total_savings_gb = total_savings_bytes / (1024 * 1024 * 1024)

    summary = {
        'total_scanned': len(subset),
        'games_with_embedded_loc': len(results),
        'total_estimated_savings_bytes': total_savings_bytes,
        'total_estimated_savings_gb': total_savings_gb,
        'elapsed_seconds': elapsed,
        'results': results
    }

    with open(output_file, 'w', encoding='utf-8') as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)

    print("\n" + "="*60)
    print(f"BATCH COMPLETE in {elapsed:.1f}s")
    print(f"Games with embedded localization: {len(results)} / {len(subset)}")
    print(f"Total potential savings: {total_savings_gb:.2f} GB")
    print(f"Saved results to: {output_file}")
    print("="*60)

if __name__ == '__main__':
    main()
