import json

en_file = "locales/en.json"
with open(en_file, "r", encoding="utf-8") as f:
    en_data = json.load(f)

en_keys_order = list(en_data["strings"].keys())

monitoring_translations = {
    "th": {
        "settings_section_monitoring": "การตรวจสอบพื้นหลัง",
        "watch_enabled_label": "เปิดใช้งานการตรวจสอบการอัปเดตเบื้องหลัง",
        "watch_enabled_hint": "ตรวจสอบไฟล์ manifest ของเกมเพื่อตรวจจับการอัปเดตจาก launcher และสแกนซ้ำหรือตัดแต่งอัตโนมัติ",
        "watch_autostart_label": "เริ่มการตรวจสอบโดยอัตโนมัติพร้อมกับ Windows",
        "watch_autostart_hint": "เปิดใช้งาน gametrimmer-watch ในพื้นหลังเมื่อผู้ใช้เข้าสู่ระบบ",
        "watch_mode_label": "โหมดการทำงานเมื่อมีการอัปเดต",
        "watch_mode_interactive": "การแจ้งเตือนแบบโต้ตอบ",
        "watch_mode_interactive_hint": "แสดงการแจ้งเตือนของ Windows Toast พร้อมปุ่มเพื่อตัดแต่งไฟล์ที่ดาวน์โหลดมาใหม่",
        "watch_mode_autotrim": "ตัดแต่งอัตโนมัติแบบเงียบ",
        "watch_mode_autotrim_hint": "ตัดแต่งเกมที่เพิ่งอัปเดตใหม่โดยอัตโนมัติอย่างเงียบๆ โดยไม่ต้องถาม",
        "watch_mode_passive": "แสดงเฉพาะป้ายสถานะ",
        "watch_mode_passive_hint": "อัปเดตป้ายสถานะใน GameTrimmer โดยไม่มีการลบอัตโนมัติหรือการแจ้งเตือน Toast",
        "watch_daemon_status_running": "ทำงานอยู่ (เชื่อมต่อ IPC แล้ว)",
        "watch_daemon_status_stopped": "หยุดทำงาน (daemon ไม่ได้ทำงานอยู่)",
        "btn_watch_rescan_now": "ตรวจสอบ / สแกนซ้ำทันที"
    },
    "vi": {
        "settings_section_monitoring": "Giám sát nền",
        "watch_enabled_label": "Bật giám sát cập nhật trong nền",
        "watch_enabled_hint": "Theo dõi tệp manifest của trò chơi để phát hiện cập nhật launcher và tự động quét lại hoặc dọn dẹp.",
        "watch_autostart_label": "Khởi động giám sát cùng Windows",
        "watch_autostart_hint": "Khởi chạy gametrimmer-watch trong nền khi người dùng đăng nhập.",
        "watch_mode_label": "Chế độ phản hồi cập nhật",
        "watch_mode_interactive": "Thông báo tương tác",
        "watch_mode_interactive_hint": "Hiển thị thông báo Windows Toast kèm nút để dọn dẹp các tệp vừa tải lại.",
        "watch_mode_autotrim": "Tự động dọn dẹp âm thầm",
        "watch_mode_autotrim_hint": "Âm thầm dọn dẹp lại các trò chơi vừa cập nhật mà không cần hỏi.",
        "watch_mode_passive": "Chỉ hiện huy hiệu thụ động",
        "watch_mode_passive_hint": "Cập nhật huy hiệu trạng thái trong GameTrimmer mà không tự động xóa hoặc hiện thông báo Toast.",
        "watch_daemon_status_running": "Đang hoạt động (Đã kết nối IPC)",
        "watch_daemon_status_stopped": "Đã dừng (Daemon không chạy)",
        "btn_watch_rescan_now": "Kiểm tra / Quét lại ngay"
    },
    "tr": {
        "settings_section_monitoring": "Arka plan izleme",
        "watch_enabled_label": "Arka plan güncelleme izlemesini etkinleştir",
        "watch_enabled_hint": "Başlatıcı güncellemeleri için oyun manifestlerini izler ve otomatik olarak yeniden tarar veya temizler.",
        "watch_autostart_label": "Windows ile otomatik başlat",
        "watch_autostart_hint": "Kullanıcı oturum açtığında gametrimmer-watch'ı arka planda başlatır.",
        "watch_mode_label": "Güncelleme eylem modu",
        "watch_mode_interactive": "Etkileşimli bildirim",
        "watch_mode_interactive_hint": "Yeniden indirilen dosyaları temizlemek için bir düğme içeren Windows Bildirimi gösterir.",
        "watch_mode_autotrim": "Sessiz otomatik temizleme",
        "watch_mode_autotrim_hint": "Yeni güncellenen oyunları sormadan sessizce yeniden temizler.",
        "watch_mode_passive": "Yalnızca pasif rozet",
        "watch_mode_passive_hint": "Otomatik silme veya bildirim olmadan GameTrimmer'daki durum rozetlerini günceller.",
        "watch_daemon_status_running": "Etkin (IPC bağlandı)",
        "watch_daemon_status_stopped": "Durduruldu (arka plan hizmeti çalışmıyor)",
        "btn_watch_rescan_now": "Şimdi kontrol et / yeniden tara"
    },
    "ar": {
        "settings_section_monitoring": "المراقبة في الخلفية",
        "watch_enabled_label": "تفعيل مراقبة التحديثات في الخلفية",
        "watch_enabled_hint": "يراقب ملفات تعريف الألعاب لتتبع تحديثات المشغلات وإعادة الفحص أو الحذف تلقائياً.",
        "watch_autostart_label": "بدء المراقبة تلقائياً مع تشغيل Windows",
        "watch_autostart_hint": "تشغيل gametrimmer-watch في الخلفية عند تسجيل دخول المستخدم.",
        "watch_mode_label": "نمط التعامل مع التحديثات",
        "watch_mode_interactive": "إشعار تفاعلي",
        "watch_mode_interactive_hint": "يعرض إشعار Windows Toast مع زر لتنظيف الملفات المعاد تحميلها.",
        "watch_mode_autotrim": "تنظيف تلقائي صامت",
        "watch_mode_autotrim_hint": "يعيد تنظيف الألعاب المحدثة حديثاً بصمت دون طلب تأكيد.",
        "watch_mode_passive": "شارة حالة سلبية فقط",
        "watch_mode_passive_hint": "يحدّث شارات الحالة داخل GameTrimmer دون حذف تلقائي أو إشعارات.",
        "watch_daemon_status_running": "نشط (متصل عبر IPC)",
        "watch_daemon_status_stopped": "متوقف (الخدمة الخلفية لا تعمل)",
        "btn_watch_rescan_now": "فحص / إعادة الفحص الآن"
    }
}

for lang, additions in monitoring_translations.items():
    path = f"locales/{lang}.json"
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    
    strings = data["strings"]
    strings.update(additions)
    
    # Re-order to match en_keys_order exactly
    ordered_strings = {}
    for k in en_keys_order:
        ordered_strings[k] = strings.get(k, en_data["strings"][k])
    
    data["strings"] = ordered_strings
    
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)
        f.write("\n")
    print(f"Updated {lang} -> {len(ordered_strings)} keys")
