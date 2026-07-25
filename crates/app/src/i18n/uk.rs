//! Ukrainian strings - the original reference text, copied verbatim from
//! where it used to be hardcoded throughout `crates/app/src`.

use super::Strings;

pub(super) const STRINGS: Strings = Strings {
    btn_scan_libraries: "Сканувати бібліотеки",
    btn_cancel: "Скасувати",
    btn_export: "Експортувати…",
    btn_settings: "Налаштування…",

    btn_select_all: "Вибрати все",
    btn_deselect_all: "Зняти вибір",
    btn_delete_selected: "Видалити вибране",
    profile_label: "Профіль:",
    profile_cautious: "Обережний",
    profile_balanced: "Збалансований",
    profile_aggressive: "Агресивний",
    profile_custom: "Налаштувати",
    profile_hint: "Що позначається наперед. Обережний: лише те, чого лаунчер не поверне \
         (осиротілі рештки, бонуси, документація). Збалансований: + мови поза вашим keep-list. \
         Агресивний: + усе з довірою від 70%. Налаштувати: звичайний поріг 85% — решту обираєте \
         вручну. Перемикання перепозначає поточні знахідки.",

    plan_heading: "План дій",
    plan_show_all: "Показати все",
    btn_card_view: "Переглянути",
    btn_card_remove: "Прибрати",

    elevation_heading: "Пришвидшити сканування?",
    elevation_body: "Швидке сканування читає файлову таблицю NTFS ($MFT) напряму, як інструмент \
         Everything, і для цього потрібні права адміністратора. Без них сканування \
         працюватиме повільніше (звичайний обхід тек).",
    btn_continue_without_elevation: "Продовжити без прискорення",
    btn_relaunch_elevated: "Перезапустити від імені адміністратора",
    confirm_delete_heading: "Підтвердження видалення",
    confirm_label_permanent: "Видалити безповоротно",
    confirm_label_recycle: "Перемістити в Кошик",
    remember_delete_method: "Запам'ятати мій вибір",
    remove_summary_heading: "Результат видалення",
    btn_close: "Закрити",
    confirm_clear_heading: "Очистити базу даних?",
    confirm_clear_body: "Усі результати сканування та журнал операцій буде безповоротно \
         вилучено з бази даних. Файли на диску не зачіпаються, бібліотеки та налаштування \
         зберігаються. Це неможливо скасувати — щоб знову побачити результати, потрібно \
         повторно просканувати бібліотеки.",
    btn_confirm_clear: "Очистити базу даних",

    settings_heading: "Налаштування",
    advanced_section: "Додатково",
    delete_method_label: "Спосіб видалення файлів:",
    delete_method_permanent_label: "Остаточне видалення (найшвидше)",
    delete_method_permanent_hint:
        "Файли видаляються безповоротно. Якщо видалиться щось потрібне — \
         гру завжди можна перевстановити з магазину.",
    delete_method_recycle_label: "У Кошик Windows (повільніше)",
    delete_method_recycle_hint: "Файли можна відновити з Кошика, доки його не очищено.",
    database_label: "База даних:",
    btn_compact_database: "Стиснути базу даних",
    compact_hint: "Звільняє місце, яке база даних більше не використовує після видалень. \
         Виконується, лише якщо звільниться щонайменше 25% обсягу.",
    btn_clear_database: "Очистити базу даних",
    clear_hint: "Безповоротно вилучає з бази даних усі результати сканування та журнал \
         операцій. Файли на диску не зачіпаються; бібліотеки та налаштування зберігаються.",
    rules_label: "Правила аналізу:",
    btn_export_rules: "Експортувати правила",
    btn_import_rules: "Імпортувати правила",
    rules_hint: "Експорт зберігає rules.json і l10n_rules.json у вибрану теку — основа для \
         власних правил чи правил спільноти. Імпорт об'єднує вибрані файли з поточними \
         правилами (нові додаються, збіги оновлюються) і зберігає їх поруч із програмою; \
         попередні файли залишаються як *.bak. Зміни діятимуть з наступного сканування.",
    running_ellipsis: "Виконується...",
    keep_languages_label: "Мови, які ніколи не позначаються:",
    keep_languages_hint: "Файли, визначені як такі, що належать позначеній мові, ніколи не \
         пропонуються до видалення. Принаймні одна мова має лишатися позначеною. \
         Зміни діятимуть з наступного сканування.",
    scan_routing_label: "Спосіб перебору файлів під час сканування:",
    scan_routing_auto_label: "Автоматично (рекомендовано)",
    scan_routing_auto_hint: "Використовує швидкий MFT-індекс на жорстких дисках і звичайний \
         обхід тек на SSD/NVMe — залежно від того, що швидше для конкретного носія.",
    scan_routing_force_mft_label: "Завжди використовувати MFT-індекс",
    scan_routing_force_mft_hint: "Використовує MFT-індекс навіть на SSD/NVMe, де обхід тек \
         зазвичай швидший. Потребує прав адміністратора; томи, які неможливо прочитати цим \
         способом (без підвищення прав, мережеві диски, junction/symlink), усе одно \
         повертаються до обходу тек. Діятиме з наступного сканування.",
    scan_routing_force_walkdir_label: "Завжди обходити теки",
    scan_routing_force_walkdir_hint: "Повністю пропускає MFT-індекс і завжди сканує обходом тек, \
         навіть на жорстких дисках, де MFT-індекс зазвичай швидший. Діятиме з наступного \
         сканування.",
    app_language_label: "Мова застосунку:",
    lang_name_en: "Англійська",
    lang_name_uk: "Українська",
    theme_label: "Тема:",
    theme_system_label: "Системна (за Windows)",
    theme_light_label: "Світла",
    theme_dark_label: "Темна",
    categories_label: "Категорії знахідок сканування:",
    categories_hint: "Непозначені категорії повністю пропускаються під час сканування — їхні \
         файли ніколи не аналізуються, не показуються й не зберігаються. \
         Принаймні одна категорія має лишатися позначеною. Зміни діятимуть \
         з наступного сканування.",
    logging_label: "Діагностичний журнал",
    logging_checkbox: "Писати діагностичний журнал (gametrimmer.log поруч із застосунком)",
    logging_hint: "Лише для аналізу проблем: записує помилки та події сканування. \
         Типово вимкнено.",

    libraries_header: "Бібліотеки",
    btn_add_folder: "Додати теку...",
    picking_folder: "Вибір теки...",
    no_libraries_registered: "Бібліотек ще не зареєстровано.",
    btn_remove: "Прибрати",

    scanning_in_progress: "Сканування триває...",
    no_findings_hint: "Немає знахідок. Натисніть \u{ab}Сканувати бібліотеки\u{bb}, щоб почати.",
    col_language: "Мова",
    col_files: "Файлів",
    col_size: "Розмір",
    col_confidence: "Довіра",
    col_name: "Назва",

    ctx_reveal_in_explorer: "Відкрити в Провіднику",
    ctx_open_with: "Відкрити за допомогою\u{2026}",
    ctx_copy_path: "Копіювати шлях",

    add_library_dialog_title: "Виберіть теку бібліотеки",
    export_dialog_title: "Експорт результатів аналізу",
    text_file_filter_label: "Текстовий файл",
    rules_export_dialog_title: "Виберіть теку для експорту правил",
    rules_import_dialog_title: "Виберіть файли правил для імпорту",
    rules_import_filter_label: "Правила GameTrimmer (JSON)",

    no_db_path: "Немає шляху до бази даних.",
    db_path_error: "Помилка визначення шляху до бази даних.",
    detecting_libraries: "Виявлення ігрових бібліотек...",
    preparing_database: "Підготовка бази даних...",
    loading_previous_scan: "Завантаження результатів попереднього сканування...",
    deleting_selected_files: "Видалення вибраних файлів...",
    compacting_database: "Стискання бази даних...",
    clearing_database: "Очищення бази даних...",
    scan_cancelled: "Сканування скасовано.",
    deletion_completed: "Видалення завершено.",
    database_compacted: "Базу даних стиснуто.",
    database_cleared: "Базу даних очищено.",
    settings_not_saved_no_db: "Налаштування не збережено: немає шляху до бази даних.",

    verb_scan: "Сканування",
    verb_analyze: "Аналіз",
    verb_delete: "Видалення",
    verb_compact: "Стискання бази даних",
    verb_clear: "Очищення бази даних",

    category_redist: "Дистрибутиви",
    category_docs: "Документація і довідкові матеріали",
    category_bonus: "Бонусні матеріали",
    category_loc: "Файли локалізацій",
    category_other: "Інше",
    category_orphan: "Осиротіле",

    orphan_branch_label: "Осиротілі рештки",

    unit_gb: "ГБ",
    unit_mb: "МБ",
    unit_kb: "КБ",
    unit_b: "Б",

    csv_yes: "так",
    csv_no: "ні",
};
