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

    settings_section_general: "Загальні",
    settings_section_scanning: "Сканування",
    settings_section_selection: "Вибір і видалення",
    settings_section_rules: "Правила",
    settings_section_data: "Дані й діагностика",
    btn_done: "Готово",
    btn_restore_defaults: "Відновити типові",
    badge_immediately: "Одразу",
    badge_next_scan: "З наступного сканування",
    badge_next_delete: "З наступного видалення",
    default_profile_label: "Профіль, з якого починає нове сканування",
    default_profile_hint: "Не змінює того, що зараз позначено в дереві, — лише те, з чого \
         почнеться наступне сканування.",
    confirm_behavior_label: "Питати перед видаленням",
    confirm_always_label: "Завжди",
    confirm_only_1gb_label: "Лише понад 1 ГБ",
    confirm_never_label: "Ніколи",
    confirm_behavior_hint: "Коли показувати підтвердження перед видаленням.",
    selection_independent_switches_hint: "Що сканується (розділ «Сканування»), що позначається \
         автоматично (профіль вище) і як воно видаляється (метод вище) — три незалежні \
         перемикачі: зміна одного не чіпає двох інших.",
    rules_pack_category_label: "Правила категорій (rules.json)",
    rules_pack_lang_label: "Мовний пакет (l10n_rules.json)",
    rules_valid_label: "Справний",
    rules_invalid_label: "Не читається",
    db_path_label: "Файл бази даних:",
    btn_copy: "Копіювати",
    btn_open_folder: "Відкрити теку",
    danger_zone_label: "НЕБЕЗПЕЧНА ЗОНА",

    disabled_busy: "Триває фонова операція",
    disabled_no_findings: "Спершу проскануйте бібліотеки",
    disabled_no_selection: "Нічого не вибрано",
    disabled_export_running: "Експорт уже виконується",

    profile_label: "Профіль:",
    profile_cautious: "Обережний",
    profile_balanced: "Збалансований",
    profile_aggressive: "Агресивний",
    // The name of a state, not a command. "Налаштувати" read as an
    // instruction to open an editor (audit §5.6).
    profile_custom: "Власний",
    profile_hint: "Що позначається наперед. Обережний: лише те, чого лаунчер не поверне \
         (осиротілі рештки, бонуси, документація). Збалансований: + мови поза вашим keep-list. \
         Агресивний: + усе з довірою від 70%. Власний: звичайний поріг 85% — решту обираєте \
         вручну. Перемикання перепозначає поточні знахідки.",

    plan_filter_label: "Показувати:",
    plan_filter_all: "усі категорії",
    btn_remove_category: "Прибрати всю категорію",
    search_hint: "Пошук за назвою\u{2026}",

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
    // Not "Завжди": elevation, volume-letter and canonical-path gates all
    // still send a game to the folder walk under this mode (audit §6.8).
    scan_routing_force_mft_label: "Надавати перевагу MFT, де доступно",
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
