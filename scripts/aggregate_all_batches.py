import os
import json

def aggregate_batches():
    batches = [
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_1.json",
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_2.json",
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_3.json",
        r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\scan_batch_4.json"
    ]
    
    all_results = []
    total_scanned = 0
    available_batches = 0
    
    for b in batches:
        if os.path.exists(b):
            available_batches += 1
            with open(b, 'r', encoding='utf-8') as f:
                data = json.load(f)
                total_scanned += data.get('total_scanned', 0)
                all_results.extend(data.get('results', []))
                
    # Sort by savings descending
    all_results.sort(key=lambda x: x.get('total_estimated_savings', 0), reverse=True)
    
    total_savings_bytes = sum(x.get('total_estimated_savings', 0) for x in all_results)
    total_savings_gb = total_savings_bytes / (1024 * 1024 * 1024)
    
    # Engine breakdown
    by_engine = {}
    by_method = {}
    
    for r in all_results:
        engines = r.get('engines_detected', ['Other / Custom'])
        for eng in engines:
            by_engine[eng] = by_engine.get(eng, {'count': 0, 'savings': 0})
            by_engine[eng]['count'] += 1
            by_engine[eng]['savings'] += r.get('total_estimated_savings', 0)
            
        methods = r.get('methods_available', ['Other'])
        for m in methods:
            by_method[m] = by_method.get(m, {'count': 0, 'savings': 0})
            by_method[m]['count'] += 1
            by_method[m]['savings'] += r.get('total_estimated_savings', 0)

    summary = {
        'available_batches': available_batches,
        'total_scanned_games': total_scanned,
        'games_with_embedded_localization': len(all_results),
        'total_estimated_savings_bytes': total_savings_bytes,
        'total_estimated_savings_gb': total_savings_gb,
        'by_engine': {k: {'count': v['count'], 'savings_gb': round(v['savings'] / (1024**3), 2)} for k, v in by_engine.items()},
        'by_method': {k: {'count': v['count'], 'savings_gb': round(v['savings'] / (1024**3), 2)} for k, v in by_method.items()},
        'top_games': []
    }
    
    for r in all_results[:60]:
        summary['top_games'].append({
            'name': r['game_name'],
            'path': r['game_path'],
            'total_size_gb': round(r['total_game_size'] / (1024**3), 2),
            'savings_gb': round(r['total_estimated_savings'] / (1024**3), 2),
            'savings_mb': round(r['total_estimated_savings'] / (1024**2), 1),
            'engines': r.get('engines_detected', []),
            'methods': r.get('methods_available', []),
            'archive_count': len(r.get('archives_found', []))
        })
        
    out_path = r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\temp\aggregate_scan_summary.json"
    with open(out_path, 'w', encoding='utf-8') as f:
        json.dump(summary, f, ensure_ascii=False, indent=2)
        
    print(f"Aggregated {len(all_results)} games across {available_batches} batches.")
    print(f"Total potential space savings: {total_savings_gb:.2f} GB ({total_savings_bytes:,} bytes)")
    print(f"Saved aggregate report to: {out_path}")
    return summary

if __name__ == '__main__':
    aggregate_batches()
