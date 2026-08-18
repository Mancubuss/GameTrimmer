import os
import json
import re

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

def has_language_in_filename(name):
    """Returns True if the file name itself contains language identifiers (which standard GameTrimmer can already detect)."""
    base = os.path.basename(name).lower()
    for tag in NON_ENGLISH_LANG_TAGS:
        pattern = r'(^|[_\-./\\ ])' + re.escape(tag) + r'([_\-./\\ ]|$)'
        if re.search(pattern, base):
            return True
    return False

def verify_and_filter_results():
    batches = [
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_1.json",
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_2.json",
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_3.json",
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_4.json"
    ]
    
    true_monolith_games = []
    
    for b in batches:
        if not os.path.exists(b):
            continue
        with open(b, 'r', encoding='utf-8') as f:
            data = json.load(f)
            
        for g in data.get('results', []):
            game_name = g['game_name']
            game_path = g['game_path']
            archives = g.get('archives_found', [])
            
            # Filter archives: ONLY keep archives whose own filename has NO language tag!
            true_monolith_archives = []
            
            for arc in archives:
                arc_file = arc.get('file', '')
                full_path = arc.get('full_path', '')
                info = arc.get('info', {})
                arc_type = info.get('type', '')
                
                # If archive name has language (e.g. German.pak, lang_ru.archive, prelude_vo_german.vpk, Bink video with _rus.bik),
                # GameTrimmer matches it directly. Exclude from monolith list!
                if has_language_in_filename(arc_file):
                    continue
                if 'Separate Lang' in arc_type or 'Bink Video Localized Cutscenes' in arc_type or 'Source VPK Lang Pack' in arc_type:
                    continue
                    
                # This is a TRUE monolith archive containing multiple languages inside
                true_monolith_archives.append(arc)
                
            if true_monolith_archives:
                monolith_savings = sum(a['info'].get('estimated_savings', 0) for a in true_monolith_archives)
                if monolith_savings > 10 * 1024 * 1024: # > 10 MB
                    true_monolith_games.append({
                        'name': game_name,
                        'path': game_path,
                        'total_game_size': g.get('total_game_size', 0),
                        'monolith_savings': monolith_savings,
                        'archives': true_monolith_archives,
                        'engines': g.get('engines_detected', [])
                    })
                    
    true_monolith_games.sort(key=lambda x: x['monolith_savings'], reverse=True)
    
    total_savings = sum(g['monolith_savings'] for g in true_monolith_games)
    
    out_file = r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\true_monoliths_verified.json"
    with open(out_file, 'w', encoding='utf-8') as f:
        json.dump({
            'total_true_monolith_games': len(true_monolith_games),
            'total_savings_gb': round(total_savings / (1024**3), 2),
            'games': true_monolith_games
        }, f, ensure_ascii=False, indent=2)
        
    print(f"Verified True Monoliths: {len(true_monolith_games)} games.")
    print(f"Real Monolith Space Savings: {total_savings / (1024**3):.2f} GB")
    print(f"Saved verified list to: {out_file}")

if __name__ == '__main__':
    verify_and_filter_results()
