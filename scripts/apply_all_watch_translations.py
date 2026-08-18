import json

en_file = 'locales/en.json'
with open(en_file, 'r', encoding='utf-8') as f:
    en_data = json.load(f)

canonical_order = list(en_data['strings'].keys())

watch_translations = {
    "de": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktiv)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Pausiert)",
        "watch_tray_menu_open": "GameTrimmer öffnen",
        "watch_tray_menu_check_now": "Jetzt prüfen",
        "watch_tray_menu_pause": "Überwachung pausieren",
        "watch_tray_menu_resume": "Überwachung fortsetzen",
        "watch_tray_menu_exit": "Beenden",
        "watch_toast_updated_transition": "{name} wurde aktualisiert ({old} → {new}). Klicken zum Bereinigen.",
        "watch_toast_updated_build": "{name} wurde aktualisiert (Build {new}). Klicken zum Bereinigen.",
        "watch_toast_files_changed": "Dateien von {name} wurden geändert. Klicken zum Bereinigen.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "fr": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Actif)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (En pause)",
        "watch_tray_menu_open": "Ouvrir GameTrimmer",
        "watch_tray_menu_check_now": "Vérifier maintenant",
        "watch_tray_menu_pause": "Mettre en pause la surveillance",
        "watch_tray_menu_resume": "Reprendre la surveillance",
        "watch_tray_menu_exit": "Quitter",
        "watch_toast_updated_transition": "{name} a été mis à jour ({old} → {new}). Cliquez pour nettoyer.",
        "watch_toast_updated_build": "{name} a été mis à jour (version {new}). Cliquez pour nettoyer.",
        "watch_toast_files_changed": "Les fichiers de {name} ont changé. Cliquez pour nettoyer.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "it": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Attivo)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (In pausa)",
        "watch_tray_menu_open": "Apri GameTrimmer",
        "watch_tray_menu_check_now": "Controlla ora",
        "watch_tray_menu_pause": "Sospendi monitoraggio",
        "watch_tray_menu_resume": "Riprendi monitoraggio",
        "watch_tray_menu_exit": "Esci",
        "watch_toast_updated_transition": "{name} è stato aggiornato ({old} → {new}). Fai clic per ripulire.",
        "watch_toast_updated_build": "{name} è stato aggiornato (build {new}). Fai clic per ripulire.",
        "watch_toast_files_changed": "I file di {name} sono cambiati. Fai clic per ripulire.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "es": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Activo)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (En pausa)",
        "watch_tray_menu_open": "Abrir GameTrimmer",
        "watch_tray_menu_check_now": "Comprobar ahora",
        "watch_tray_menu_pause": "Pausar monitorización",
        "watch_tray_menu_resume": "Reanudar monitorización",
        "watch_tray_menu_exit": "Salir",
        "watch_toast_updated_transition": "{name} se ha actualizado ({old} → {new}). Haz clic para limpiar.",
        "watch_toast_updated_build": "{name} se ha actualizado (versión {new}). Haz clic para limpiar.",
        "watch_toast_files_changed": "Los archivos de {name} han cambiado. Haz clic para limpiar.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "es-419": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Activo)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (En pausa)",
        "watch_tray_menu_open": "Abrir GameTrimmer",
        "watch_tray_menu_check_now": "Comprobar ahora",
        "watch_tray_menu_pause": "Pausar monitoreo",
        "watch_tray_menu_resume": "Reanudar monitoreo",
        "watch_tray_menu_exit": "Salir",
        "watch_toast_updated_transition": "{name} se actualizó ({old} → {new}). Haz clic para limpiar.",
        "watch_toast_updated_build": "{name} se actualizó (build {new}). Haz clic para limpiar.",
        "watch_toast_files_changed": "Los archivos de {name} cambiaron. Haz clic para limpiar.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "pt-BR": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Ativo)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Pausado)",
        "watch_tray_menu_open": "Abrir GameTrimmer",
        "watch_tray_menu_check_now": "Verificar agora",
        "watch_tray_menu_pause": "Pausar monitoramento",
        "watch_tray_menu_resume": "Retomar monitoramento",
        "watch_tray_menu_exit": "Sair",
        "watch_toast_updated_transition": "{name} foi atualizado ({old} → {new}). Clique para limpar.",
        "watch_toast_updated_build": "{name} foi atualizado (build {new}). Clique para limpar.",
        "watch_toast_files_changed": "Arquivos de {name} foram alterados. Clique para limpar.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "pt": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Ativo)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Em pausa)",
        "watch_tray_menu_open": "Abrir GameTrimmer",
        "watch_tray_menu_check_now": "Verificar agora",
        "watch_tray_menu_pause": "Pausar monitorização",
        "watch_tray_menu_resume": "Retomar monitorização",
        "watch_tray_menu_exit": "Sair",
        "watch_toast_updated_transition": "{name} foi atualizado ({old} → {new}). Clique para limpar.",
        "watch_toast_updated_build": "{name} foi atualizado (versão {new}). Clique para limpar.",
        "watch_toast_files_changed": "Ficheiros de {name} foram alterados. Clique para limpar.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "nl": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Actief)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Gepauzeerd)",
        "watch_tray_menu_open": "GameTrimmer openen",
        "watch_tray_menu_check_now": "Nu controleren",
        "watch_tray_menu_pause": "Monitoring pauzeren",
        "watch_tray_menu_resume": "Monitoring hervatten",
        "watch_tray_menu_exit": "Afsluiten",
        "watch_toast_updated_transition": "{name} is bijgewerkt ({old} → {new}). Klik om op te schonen.",
        "watch_toast_updated_build": "{name} is bijgewerkt (build {new}). Klik om op te schonen.",
        "watch_toast_files_changed": "Bestanden van {name} zijn gewijzigd. Klik om op te schonen.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "da": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktiv)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Sat på pause)",
        "watch_tray_menu_open": "Åbn GameTrimmer",
        "watch_tray_menu_check_now": "Tjek nu",
        "watch_tray_menu_pause": "Sæt overvågning på pause",
        "watch_tray_menu_resume": "Genoptag overvågning",
        "watch_tray_menu_exit": "Afslut",
        "watch_toast_updated_transition": "{name} blev opdateret ({old} → {new}). Klik for at rydde op.",
        "watch_toast_updated_build": "{name} blev opdateret (build {new}). Klik for at rydde op.",
        "watch_toast_files_changed": "Filer for {name} er ændret. Klik for at rydde op.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "sv": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktiv)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Pausad)",
        "watch_tray_menu_open": "Öppna GameTrimmer",
        "watch_tray_menu_check_now": "Kontrollera nu",
        "watch_tray_menu_pause": "Pausa övervakning",
        "watch_tray_menu_resume": "Återuppta övervakning",
        "watch_tray_menu_exit": "Avsluta",
        "watch_toast_updated_transition": "{name} uppdaterades ({old} → {new}). Klicka för att rensa.",
        "watch_toast_updated_build": "{name} uppdaterades (build {new}). Klicka för att rensa.",
        "watch_toast_files_changed": "Filer för {name} har ändrats. Klicka för att rensa.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "no": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktiv)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Pauset)",
        "watch_tray_menu_open": "Åpne GameTrimmer",
        "watch_tray_menu_check_now": "Sjekk nå",
        "watch_tray_menu_pause": "Sett overvåking på pause",
        "watch_tray_menu_resume": "Gjenoppta overvåking",
        "watch_tray_menu_exit": "Avslutt",
        "watch_toast_updated_transition": "{name} ble oppdatert ({old} → {new}). Klikk for å rydde.",
        "watch_toast_updated_build": "{name} ble oppdatert (build {new}). Klikk for å rydde.",
        "watch_toast_files_changed": "Filer for {name} er endret. Klikk for å rydde.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "fi": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktiivinen)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Keskeytetty)",
        "watch_tray_menu_open": "Avaa GameTrimmer",
        "watch_tray_menu_check_now": "Tarkista nyt",
        "watch_tray_menu_pause": "Keskeytä valvonta",
        "watch_tray_menu_resume": "Jatka valvontaa",
        "watch_tray_menu_exit": "Lopeta",
        "watch_toast_updated_transition": "{name} päivitettiin ({old} → {new}). Klikkaa siivotaksesi.",
        "watch_toast_updated_build": "{name} päivitettiin (versio {new}). Klikkaa siivotaksesi.",
        "watch_toast_files_changed": "Pelin {name} tiedostot muuttuivat. Klikkaa siivotaksesi.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "pl": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktywny)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Wstrzymany)",
        "watch_tray_menu_open": "Otwórz GameTrimmer",
        "watch_tray_menu_check_now": "Sprawdź teraz",
        "watch_tray_menu_pause": "Wstrzymaj monitorowanie",
        "watch_tray_menu_resume": "Wznów monitorowanie",
        "watch_tray_menu_exit": "Zakończ",
        "watch_toast_updated_transition": "Gra {name} została zaktualizowana ({old} → {new}). Kliknij, aby oczyścić.",
        "watch_toast_updated_build": "Gra {name} została zaktualizowana (kompilacja {new}). Kliknij, aby oczyścić.",
        "watch_toast_files_changed": "Pliki gry {name} uległy zmianie. Kliknij, aby oczyścić.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "cs": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktivní)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Pozastaveno)",
        "watch_tray_menu_open": "Otevřít GameTrimmer",
        "watch_tray_menu_check_now": "Zkontrolovat nyní",
        "watch_tray_menu_pause": "Pozastavit sledování",
        "watch_tray_menu_resume": "Obnovit sledování",
        "watch_tray_menu_exit": "Ukončit",
        "watch_toast_updated_transition": "Hra {name} byla aktualizována ({old} → {new}). Kliknutím vyčistíte.",
        "watch_toast_updated_build": "Hra {name} byla aktualizována (sestavení {new}). Kliknutím vyčistíte.",
        "watch_toast_files_changed": "Soubory hry {name} se změnily. Kliknutím vyčistíte.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "hu": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Aktív)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Szüneteltetve)",
        "watch_tray_menu_open": "GameTrimmer megnyitása",
        "watch_tray_menu_check_now": "Ellenőrzés most",
        "watch_tray_menu_pause": "Figyelés szüneteltetése",
        "watch_tray_menu_resume": "Figyelés folytatása",
        "watch_tray_menu_exit": "Kilépés",
        "watch_toast_updated_transition": "A(z) {name} frissült ({old} → {new}). Kattintson a tisztításhoz.",
        "watch_toast_updated_build": "A(z) {name} frissült ({new} build). Kattintson a tisztításhoz.",
        "watch_toast_files_changed": "A(z) {name} fájljai megváltoztak. Kattintson a tisztításhoz.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "bg": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Активен)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (На пауза)",
        "watch_tray_menu_open": "Отваряне на GameTrimmer",
        "watch_tray_menu_check_now": "Провери сега",
        "watch_tray_menu_pause": "Пауза на наблюдението",
        "watch_tray_menu_resume": "Възобновяване на наблюдението",
        "watch_tray_menu_exit": "Изход",
        "watch_toast_updated_transition": "{name} беше актуализирана ({old} → {new}). Щракнете за почистване.",
        "watch_toast_updated_build": "{name} беше актуализирана (билд {new}). Щракнете за почистване.",
        "watch_toast_files_changed": "Файловете на {name} се промениха. Щракнете за почистване.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "ro": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Activ)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (În pauză)",
        "watch_tray_menu_open": "Deschide GameTrimmer",
        "watch_tray_menu_check_now": "Verifică acum",
        "watch_tray_menu_pause": "Întrerupe monitorizarea",
        "watch_tray_menu_resume": "Reia monitorizarea",
        "watch_tray_menu_exit": "Ieșire",
        "watch_toast_updated_transition": "{name} a fost actualizat ({old} → {new}). Apasă pentru curățare.",
        "watch_toast_updated_build": "{name} a fost actualizat (build {new}). Apasă pentru curățare.",
        "watch_toast_files_changed": "Fișierele jocului {name} s-au modificat. Apasă pentru curățare.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "el": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Ενεργό)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Σε παύση)",
        "watch_tray_menu_open": "Άνοιγμα GameTrimmer",
        "watch_tray_menu_check_now": "Έλεγχος τώρα",
        "watch_tray_menu_pause": "Παύση παρακολούθησης" ,
        "watch_tray_menu_resume": "Συνέχιση παρακολούθησης",
        "watch_tray_menu_exit": "Έξοδος",
        "watch_toast_updated_transition": "Το {name} ενημερώθηκε ({old} → {new}). Κάντε κλικ για εκκαθάριση.",
        "watch_toast_updated_build": "Το {name} ενημερώθηκε (έκδοση {new}). Κάντε κλικ για εκκαθάριση.",
        "watch_toast_files_changed": "Τα αρχεία του {name} άλλαξαν. Κάντε κλικ για εκκαθάριση.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "ru": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Активен)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Приостановлен)",
        "watch_tray_menu_open": "Открыть GameTrimmer",
        "watch_tray_menu_check_now": "Проверить сейчас",
        "watch_tray_menu_pause": "Приостановить мониторинг",
        "watch_tray_menu_resume": "Возобновить мониторинг",
        "watch_tray_menu_exit": "Выход",
        "watch_toast_updated_transition": "Игра {name} обновлена ({old} → {new}). Нажмите для очистки.",
        "watch_toast_updated_build": "Игра {name} обновлена (билд {new}). Нажмите для очистки.",
        "watch_toast_files_changed": "Файлы игры {name} изменились. Нажмите для очистки.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "ja": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (有効)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (一時停止中)",
        "watch_tray_menu_open": "GameTrimmerを開く",
        "watch_tray_menu_check_now": "今すぐ確認",
        "watch_tray_menu_pause": "監視を一時停止",
        "watch_tray_menu_resume": "監視を再開",
        "watch_tray_menu_exit": "終了",
        "watch_toast_updated_transition": "{name}が更新されました ({old} → {new})。クリックして不要ファイルを削除。",
        "watch_toast_updated_build": "{name}が更新されました (ビルド {new})。クリックして不要ファイルを削除。",
        "watch_toast_files_changed": "{name}のファイルが変更されました。クリックして不要ファイルを削除。",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "ko": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (활성)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (일시 정지됨)",
        "watch_tray_menu_open": "GameTrimmer 열기",
        "watch_tray_menu_check_now": "지금 확인",
        "watch_tray_menu_pause": "모니터링 일시 정지",
        "watch_tray_menu_resume": "모니터링 재개",
        "watch_tray_menu_exit": "종료",
        "watch_toast_updated_transition": "{name} 게임이 업데이트되었습니다 ({old} → {new}). 클릭하여 불필요한 파일을 정리하세요.",
        "watch_toast_updated_build": "{name} 게임이 업데이트되었습니다 (빌드 {new}). 클릭하여 불필요한 파일을 정리하세요.",
        "watch_toast_files_changed": "{name} 게임 파일이 변경되었습니다. 클릭하여 불필요한 파일을 정리하세요.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "zh-Hans": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (运行中)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (已暂停)",
        "watch_tray_menu_open": "打开 GameTrimmer",
        "watch_tray_menu_check_now": "立即检查",
        "watch_tray_menu_pause": "暂停监控",
        "watch_tray_menu_resume": "恢复监控",
        "watch_tray_menu_exit": "退出",
        "watch_toast_updated_transition": "{name} 已更新 ({old} → {new})。点击以清理多余文件。",
        "watch_toast_updated_build": "{name} 已更新 (版本 {new})。点击以清理多余文件。",
        "watch_toast_files_changed": "{name} 文件已更改。点击以清理多余文件。",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "zh-Hant": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (執行中)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (已暫停)",
        "watch_tray_menu_open": "開啟 GameTrimmer",
        "watch_tray_menu_check_now": "立即檢查",
        "watch_tray_menu_pause": "暫停監控",
        "watch_tray_menu_resume": "繼續監控",
        "watch_tray_menu_exit": "結束",
        "watch_toast_updated_transition": "{name} 已更新 ({old} → {new})。點擊以清理多餘檔案。",
        "watch_toast_updated_build": "{name} 已更新 (版本 {new})。點擊以清理多餘檔案。",
        "watch_toast_files_changed": "{name} 檔案已變更。點擊以清理多餘檔案。",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "th": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (ทำงานอยู่)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (หยุดชั่วคราว)",
        "watch_tray_menu_open": "เปิด GameTrimmer",
        "watch_tray_menu_check_now": "ตรวจสอบทันที",
        "watch_tray_menu_pause": "หยุดการตรวจสอบชั่วคราว",
        "watch_tray_menu_resume": "ดำเนินการตรวจสอบต่อ",
        "watch_tray_menu_exit": "ออก",
        "watch_toast_updated_transition": "{name} ได้รับการอัปเดตแล้ว ({old} → {new}) คลิกเพื่อตัดแต่งและคืนพื้นที่",
        "watch_toast_updated_build": "{name} ได้รับการอัปเดตแล้ว (บิลด์ {new}) คลิกเพื่อตัดแต่งและคืนพื้นที่",
        "watch_toast_files_changed": "ไฟล์ของ {name} มีการเปลี่ยนแปลง คลิกเพื่อตัดแต่งและคืนพื้นที่",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "vi": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Đang hoạt động)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Đã tạm dừng)",
        "watch_tray_menu_open": "Mở GameTrimmer",
        "watch_tray_menu_check_now": "Kiểm tra ngay",
        "watch_tray_menu_pause": "Tạm dừng giám sát",
        "watch_tray_menu_resume": "Tiếp tục giám sát",
        "watch_tray_menu_exit": "Thoát",
        "watch_toast_updated_transition": "{name} đã được cập nhật ({old} → {new}). Nhấp để dọn dẹp và lấy lại dung lượng.",
        "watch_toast_updated_build": "{name} đã được cập nhật (bản dựng {new}). Nhấp để dọn dẹp và lấy lại dung lượng.",
        "watch_toast_files_changed": "Các tệp của {name} đã thay đổi. Nhấp để dọn dẹp và lấy lại dung lượng.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "tr": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (Etkin)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (Duraklatıldı)",
        "watch_tray_menu_open": "GameTrimmer'ı Aç",
        "watch_tray_menu_check_now": "Şimdi kontrol et",
        "watch_tray_menu_pause": "İzlemeyi duraklat",
        "watch_tray_menu_resume": "İzlemeyi sürdür",
        "watch_tray_menu_exit": "Çıkış",
        "watch_toast_updated_transition": "{name} güncellendi ({old} → {new}). Temizlemek için tıklayın.",
        "watch_toast_updated_build": "{name} güncellendi (yapı {new}). Temizlemek için tıklayın.",
        "watch_toast_files_changed": "{name} dosyaları değişti. Temizlemek için tıklayın.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "ar": {
        "watch_tray_tooltip_active": "GameTrimmer Watcher (نشط)",
        "watch_tray_tooltip_paused": "GameTrimmer Watcher (متوقف مؤقتاً)",
        "watch_tray_menu_open": "فتح GameTrimmer",
        "watch_tray_menu_check_now": "فحص الآن",
        "watch_tray_menu_pause": "إيقاف المراقبة مؤقتاً",
        "watch_tray_menu_resume": "استئناف المراقبة",
        "watch_tray_menu_exit": "خروج",
        "watch_toast_updated_transition": "تم تحديث {name} ({old} ← {new}). انقر لإعادة التنظيف واستعادة المساحة.",
        "watch_toast_updated_build": "تم تحديث {name} (الإصدار {new}). انقر لإعادة التنظيف واستعادة المساحة.",
        "watch_toast_files_changed": "تغيرت ملفات {name}. انقر لإعادة التنظيف واستعادة المساحة.",
        "watch_toast_daemon_title": "GameTrimmer Watcher"
    },
    "gametrimmer.template": {
        "watch_tray_tooltip_active": "[GameTrimmer Watcher (Active)]",
        "watch_tray_tooltip_paused": "[GameTrimmer Watcher (Paused)]",
        "watch_tray_menu_open": "[Open GameTrimmer]",
        "watch_tray_menu_check_now": "[Check now]",
        "watch_tray_menu_pause": "[Pause monitoring]",
        "watch_tray_menu_resume": "[Resume monitoring]",
        "watch_tray_menu_exit": "[Exit]",
        "watch_toast_updated_transition": "[{name} was updated ({old} → {new}). Click to re-trim and reclaim space.]",
        "watch_toast_updated_build": "[{name} was updated (build {new}). Click to re-trim and reclaim space.]",
        "watch_toast_files_changed": "[{name} files changed. Click to re-trim and reclaim space.]",
        "watch_toast_daemon_title": "[GameTrimmer Watcher]"
    }
}

for lang_id, additions in watch_translations.items():
    if lang_id == "gametrimmer.template":
        path = "locales/gametrimmer.template.json"
    else:
        path = f"locales/{lang_id}.json"
    
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    
    strings = data.get("strings", {})
    strings.update(additions)
    
    ordered_strings = {}
    for k in canonical_order:
        if k in strings:
            ordered_strings[k] = strings[k]
        elif lang_id == "gametrimmer.template":
            ordered_strings[k] = f"[{en_data['strings'][k]}]"
        else:
            ordered_strings[k] = en_data['strings'][k]
    
    data["strings"] = ordered_strings
    
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write("\n")
    
    print(f"Updated {lang_id} -> {len(ordered_strings)} keys")
