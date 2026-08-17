import urllib.request
import json
import urllib.parse
import re
import time

def fetch_pcgw_skip_intros():
    headers = {'User-Agent': 'GameTrimmer-Harvester/1.0 (contact: github.com/Mancubuss/GameTrimmer)'}
    
    # Search for pages containing "Skip intro videos"
    offset = 0
    all_pages = []
    
    print("Searching PCGW for 'Skip intro videos'...")
    while len(all_pages) < 200: # get top 200 relevant pages
        query_url = f"https://www.pcgamingwiki.com/w/api.php?action=query&list=search&srwhat=text&srsearch=%22Skip+intro+videos%22&srlimit=50&sroffset={offset}&format=json"
        req = urllib.request.Request(query_url, headers=headers)
        try:
            with urllib.request.urlopen(req) as resp:
                data = json.loads(resp.read().decode('utf-8'))
                search_results = data.get('query', {}).get('search', [])
                if not search_results:
                    break
                for res in search_results:
                    all_pages.append(res['title'])
                offset += len(search_results)
                print(f"Collected {len(all_pages)} titles...")
        except Exception as e:
            print(f"Error fetching search results: {e}")
            break
        time.sleep(0.5)

    print(f"Total candidate pages: {len(all_pages)}")
    
    file_fixes = []
    
    for i, title in enumerate(all_pages):
        parse_url = f"https://www.pcgamingwiki.com/w/api.php?action=parse&page={urllib.parse.quote(title)}&prop=wikitext&format=json"
        req = urllib.request.Request(parse_url, headers=headers)
        try:
            with urllib.request.urlopen(req) as resp:
                data = json.loads(resp.read().decode('utf-8'))
                wikitext = data.get('parse', {}).get('wikitext', {}).get('*', '')
                
                # Extract steam appid if available
                appid_match = re.search(r'\|\s*steam\s*appids?\s*=\s*([0-9,\s]+)', wikitext, re.IGNORECASE)
                steam_appids = [x.strip() for x in appid_match.group(1).split(',') if x.strip().isdigit()] if appid_match else []
                
                # Extract skip intro section
                match = re.search(r'===\s*Skip intro videos\s*===(.*?)(?===|\Z)', wikitext, re.DOTALL | re.IGNORECASE)
                if match:
                    section = match.group(1)
                    # Check if section mentions deleting, renaming, removing, or replacing files
                    is_file_method = re.search(r'(delete|rename|remove|replace|blank|empty|null|dummy|stub)', section, re.IGNORECASE)
                    if is_file_method:
                        # Extract files/folders mentioned
                        files = re.findall(r'\{\{(?:file|folder)\|([^}]+)\}\}', section, re.IGNORECASE)
                        raw_matches = re.findall(r'([A-Za-z0-9_\-\\\/]+\.(?:bik|bk2|mp4|webm|ogv|wmv|avi|usm|dat|mov|mkv))', section, re.IGNORECASE)
                        combined_files = list(set(files + raw_matches))
                        video_files = [f for f in combined_files if re.search(r'\.(bik|bk2|mp4|webm|ogv|wmv|avi|usm|dat|mov|mkv)$', f, re.IGNORECASE) or any(k in f.lower() for k in ['video', 'movie', 'intro', 'logo', 'splash', 'boot', 'legal'])]
                        
                        if video_files:
                            file_fixes.append({
                                'title': title,
                                'appids': steam_appids,
                                'files': video_files,
                                'snippet': section[:200].replace('\n', ' ')
                            })
                            print(f"[{i+1}/{len(all_pages)}] {title} (AppID: {steam_appids}) -> {video_files}")
        except Exception as e:
            # print(f"Error parsing {title}: {e}")
            pass
        time.sleep(0.3)

    print(f"\nFound {len(file_fixes)} games with explicit file-based skip intro fixes on PCGW!")
    with open("temp_pcgw_fixes.json", "w", encoding="utf-8") as f:
        json.dump(file_fixes, f, indent=2, ensure_ascii=False)

if __name__ == '__main__':
    fetch_pcgw_skip_intros()
