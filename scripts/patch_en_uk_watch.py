import json

en_file = 'locales/en.json'
uk_file = 'locales/uk.json'

with open(en_file, 'r', encoding='utf-8') as f:
    en_data = json.load(f)

with open(uk_file, 'r', encoding='utf-8') as f:
    uk_data = json.load(f)

en_add = {
    'watch_tray_tooltip_active': 'GameTrimmer Watcher (Active)',
    'watch_tray_tooltip_paused': 'GameTrimmer Watcher (Paused)',
    'watch_tray_menu_open': 'Open GameTrimmer',
    'watch_tray_menu_check_now': 'Check now',
    'watch_tray_menu_pause': 'Pause monitoring',
    'watch_tray_menu_resume': 'Resume monitoring',
    'watch_tray_menu_exit': 'Exit',
    'watch_toast_updated_transition': '{name} was updated ({old} → {new}). Click to re-trim and reclaim space.',
    'watch_toast_updated_build': '{name} was updated (build {new}). Click to re-trim and reclaim space.',
    'watch_toast_files_changed': '{name} files changed. Click to re-trim and reclaim space.',
    'watch_toast_daemon_title': 'GameTrimmer Watcher'
}

uk_add = {
    'watch_tray_tooltip_active': 'Фоновий монітор GameTrimmer (Активний)',
    'watch_tray_tooltip_paused': 'Фоновий монітор GameTrimmer (Призупинено)',
    'watch_tray_menu_open': 'Відкрити GameTrimmer',
    'watch_tray_menu_check_now': 'Перевірити зараз',
    'watch_tray_menu_pause': 'Призупинити моніторинг',
    'watch_tray_menu_resume': 'Відновити моніторинг',
    'watch_tray_menu_exit': 'Вийти',
    'watch_toast_updated_transition': '{name} оновлено ({old} → {new}). Натисніть, щоб очистити рештки.',
    'watch_toast_updated_build': '{name} оновлено (білд {new}). Натисніть, щоб очистити рештки.',
    'watch_toast_files_changed': 'Файли гри {name} змінилися. Натисніть, щоб очистити рештки.',
    'watch_toast_daemon_title': 'Фоновий монітор GameTrimmer'
}

def insert_keys(orig_dict, additions):
    res = {}
    for k, v in orig_dict.items():
        res[k] = v
        if k == 'btn_watch_rescan_now':
            for ak, av in additions.items():
                res[ak] = av
    return res

en_data['strings'] = insert_keys(en_data['strings'], en_add)
uk_data['strings'] = insert_keys(uk_data['strings'], uk_add)

with open(en_file, 'w', encoding='utf-8') as f:
    json.dump(en_data, f, ensure_ascii=False, indent=2)
    f.write('\n')

with open(uk_file, 'w', encoding='utf-8') as f:
    json.dump(uk_data, f, ensure_ascii=False, indent=2)
    f.write('\n')

print(f"EN total keys: {len(en_data['strings'])}")
print(f"UK total keys: {len(uk_data['strings'])}")
