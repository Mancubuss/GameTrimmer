import urllib.request
import json
import urllib.parse
import re
import time
from concurrent.futures import ThreadPoolExecutor, as_completed

HEADERS = {'User-Agent': 'GameTrimmer-Harvester/1.0 (contact: github.com/Mancubuss/GameTrimmer)'}

def get_all_pcgw_search_titles():
    offset = 0
    titles = []
    print("Collecting all search result titles for 'Skip intro videos'...")
    while True:
        query_url = f"https://www.pcgamingwiki.com/w/api.php?action=query&list=search&srwhat=text&srsearch=%22Skip+intro+videos%22&srlimit=50&sroffset={offset}&format=json"
        req = urllib.request.Request(query_url, headers=HEADERS)
        try:
            with urllib.request.urlopen(req) as resp:
                data = json.loads(resp.read().decode('utf-8'))
                search_results = data.get('query', {}).get('search', [])
                if not search_results:
                    break
                for item in search_results:
                    titles.append(item['title'])
                offset += len(search_results)
                print(f"Collected {len(titles)} titles so far...")
                if len(search_results) < 50:
                    break
        except Exception as e:
            print(f"Error fetching search titles at offset {offset}: {e}")
            break
        time.sleep(0.2)
    return titles

def parse_game_page(title):
    parse_url = f"https://www.pcgamingwiki.com/w/api.php?action=parse&page={urllib.parse.quote(title)}&prop=wikitext&format=json"
    req = urllib.request.Request(parse_url, headers=HEADERS)
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            data = json.loads(resp.read().decode('utf-8'))
            wikitext = data.get('parse', {}).get('wikitext', {}).get('*', '')
            
            # Extract steam appid
            appid_match = re.search(r'\|\s*steam\s*appids?\s*=\s*([0-9,\s]+)', wikitext, re.IGNORECASE)
            steam_appids = [x.strip() for x in appid_match.group(1).split(',') if x.strip().isdigit()] if appid_match else []
            
            # Extract gog appid
            gog_match = re.search(r'\|\s*gogcom\s*id\s*=\s*([0-9,\s]+)', wikitext, re.IGNORECASE)
            gog_appids = [x.strip() for x in gog_match.group(1).split(',') if x.strip().isdigit()] if gog_match else []

            match = re.search(r'===\s*Skip intro videos\s*===(.*?)(?===|\Z)', wikitext, re.DOTALL | re.IGNORECASE)
            if not match:
                return None
            
            section = match.group(1)
            is_file_method = re.search(r'(delete|rename|remove|replace|blank|empty|null|dummy|stub|mov|bik|bk2|mp4|webm|usm)', section, re.IGNORECASE)
            if not is_file_method:
                return None
            
            files = re.findall(r'\{\{(?:file|folder)\|([^}]+)\}\}', section, re.IGNORECASE)
            raw_matches = re.findall(r'([A-Za-z0-9_\-\\\/]+\.(?:bik|bk2|mp4|webm|ogv|wmv|avi|usm|dat|mov|mkv|int|mad|flv))', section, re.IGNORECASE)
            combined_files = list(set(files + raw_matches))
            video_files = [f for f in combined_files if re.search(r'\.(bik|bk2|mp4|webm|ogv|wmv|avi|usm|dat|mov|mkv|int|mad|flv)$', f, re.IGNORECASE) or any(k in f.lower() for k in ['video', 'movie', 'intro', 'logo', 'splash', 'boot', 'legal', 'ident', 'stinger'])]
            
            # Filter out non-video files or scripts (e.g. .ini, .exe, .bat, .dll unless stub)
            target_files = []
            for vf in video_files:
                clean = vf.replace('{{P|game}}\\', '').replace('{{p|game}}/', '').replace('{{p|game}}\\', '').strip()
                if clean and not clean.endswith(('.exe', '.ini', '.txt', '.bat', '.py', '.dll')):
                    target_files.append(clean)

            if target_files:
                return {
                    'title': title,
                    'steam_appids': steam_appids,
                    'gog_appids': gog_appids,
                    'files': target_files,
                    'snippet': section[:180].replace('\n', ' ')
                }
    except Exception:
        return None
    return None

def main():
    titles = get_all_pcgw_search_titles()
    print(f"Processing {len(titles)} game pages with 15 concurrent threads...")
    
    results = []
    with ThreadPoolExecutor(max_workers=15) as executor:
        future_to_title = {executor.submit(parse_game_page, t): t for t in titles}
        completed = 0
        for future in as_completed(future_to_title):
            res = future.result()
            if res:
                results.append(res)
            completed += 1
            if completed % 100 == 0 or completed == len(titles):
                print(f"Processed {completed}/{len(titles)} pages, found {len(results)} file-based skip intro entries...")
    
    print(f"\nFinal result: Extracted {len(results)} game entries with explicit file-based skip intros from PCGW!")
    with open("docs/pcgw_skip_intro_dataset.json", "w", encoding="utf-8") as f:
        json.dump(results, f, indent=2, ensure_ascii=False)
    print("Saved to docs/pcgw_skip_intro_dataset.json")

if __name__ == '__main__':
    main()
