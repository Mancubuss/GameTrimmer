# GT-EP10 — аудит імплементації `feat/epic-10-monolithic-archives`

Дата: 2026-08-20  
Гілка: `feat/epic-10-monolithic-archives`  
Перевірений HEAD: `af0604b65172af35f529ccbf4f3775c8d76f60b6` + незакомічений робочий стан  
Епік Vikunja: GT-EP10, task `309`, index `198`

> **Статус документа:** це історичний snapshot стану до remediation. Початкові
> докази та `NO-GO` нижче збережено без переписування. Актуальний стан після
> виправлень, verification build і backlog завершення наведено в додатку в кінці
> документа та у `docs/gt-ep10-remediation-2026-08-20.md`.

## Резюме

Вердикт: **NO-GO для злиття, релізу та будь-якого реального тримінгу монолітів**.

Найнебезпечніша помилка — production GUI не виконує `FindingAction::SparseZero`: він передає моноліт у звичайний delete-worker, який видаляє або переміщує до кошика **весь** `.pck/.pak/.asar/.bik`. Також anti-cheat verdict губиться між фазами сканування, невідомі мови трактуються як придатні до видалення, а заявлений rollback зберігає лише перші 64 KiB і не відновлює занулений payload.

Базові FSCTL wrappers, моделі дій, UI-представлення, PCK/ASAR synthetic parsing і частина формат-детекції існують. Однак `repak`, справжній BNK parser, RE resource traversal/loose-loader, Unity trimming, ASAR unpack і decoder-validated Bink stub не реалізовані або не підтверджені acceptance-тестами. Статуси Done для GT-140…146 не відповідають поточному коду.

## Scope тікетів

Прочитано живий вміст GT-EP10 та всіх його subtasks:

- GT-136 / task `304` — технічний spike, open;
- GT-137 / task `305` — legal/safety spike, open;
- GT-140 / task `308` — sparse core, Done;
- GT-141 / task `310` — Wwise PCK/BNK, Done;
- GT-142 / task `311` — Unreal/`repak`, Done;
- GT-143 / task `312` — Capcom RE Engine, Done;
- GT-144 / task `313` — Unity + Electron ASAR, Done;
- GT-145 / task `314` — Bink stub, Done;
- GT-146 / task `315` — Anti-Cheat Shield, Done;
- GT-375 / task `375` — повільний UI, open;
- GT-376 / task `376` — невизначені мови, open; переглянуто обидва screenshots.

## Release blockers

### P0 — GUI видаляє весь моноліт замість виконання `SparseZero`

Докази:

- `crates/app/src/app.rs:1292-1313` перетворює вибрані findings на `DeleteItem { file_id, size_on_disk }`; `FindingAction` втрачається.
- `crates/app/src/worker/delete.rs:61-64,100-112,146-149` завжди будує звичайні delete plans і викликає `PermanentDelete` або `RecycleBin`.
- `crates/core/src/cleanup.rs:69-119` уміє розрізняти `DirectDelete`, `SparseZero` і `Repack`, але production call-site для цього engine відсутній.

Сценарій: користувач ставить checkbox на archive finding і натискає «Видалити вибране». Замість занулення вибраних внутрішніх діапазонів програма видаляє/переміщує цілий контейнер. Це пряма втрата ігрових даних.

Необхідний gate: до повної інтеграції action-aware execution усі monolithic findings мають бути read-only і не мати checkbox/delete action.

### P0 — невідомі Wwise language IDs fail-open і потрапляють у destructive action

Докази:

- `crates/archive_trimmer/src/formats/wwise.rs:99-130` перетворює невідомий non-zero ID на `Language_<id>` і ставить `is_language = true`.
- `crates/core/src/worker.rs:104-140` canonicalizes це як `other` і все одно створює `SparseZero`, якщо `other` не у keep-list.

Наслідок: частково розібраний або новий формат може занулити SFX, English чи інший потрібний stream. Невідома мова мусить бути **non-trimmable** і блокувати автоматичну дію.

### P0 — anti-cheat verdict губиться перед Phase 3

Докази:

- `crates/app/src/worker/scan/persistence.rs:368-404` зберігає у `files` лише flagged files та archive candidates.
- `crates/app/src/worker/scan.rs:658-673` повторно визначає anti-cheat лише з цього урізаного набору.
- `crates/app/src/worker/scan.rs:835-836` трактує відсутній cache entry як `false`.

Типовий `EasyAntiCheat/easyanticheat_x64.dll` не є finding або archive candidate, тому зникає з Phase 3 input. Архів поруч може бути показаний як безпечний та доступний для вибору. Це суперечить GT-146.

## Високопріоритетні correctness і safety проблеми

### P1 — `can_zero_in_place` ігнорується

`crates/core/src/worker.rs:104-140` створює `SparseZero` з усіх мовних chunks, не перевіряючи `chunk.can_zero_in_place`.

- UE позначає compressed/encrypted entries як незанульовувані: `crates/archive_trimmer/src/formats/ue_pak.rs:73-87`.
- Bink явно має `can_zero_in_place = false`: `crates/archive_trimmer/src/formats/bink.rs:97-114`.

Після підключення поточного cleanup engine це пошкодить compressed UE entries і Bink-файли. Потрібна action type, що точно відповідає формату (`SparseZero`, validated stub replacement, validated repack, unsupported/read-only).

### P1 — «rollback» не відновлює змінені дані

- Snapshot читає лише перші 65 536 bytes: `crates/archive_trimmer/src/safety.rs:117-119`.
- Restore записує назад лише ці bytes: `crates/archive_trimmer/src/safety.rs:180-189`.
- Bink trim замінює весь файл: `crates/archive_trimmer/src/formats/bink.rs:167-176`.

Snapshot не містить sparse-zeroed payload і не може повернути його. Додатково:

- `original_path` і `original_size` не перевіряються, хоча `PathMismatch` оголошений;
- `<filename>.gt_snap.json` перезаписується наступним trim;
- snapshot write не є atomic/durable;
- partial failure після кількох успішних zero ranges не відкочує вже знищені ranges.

Потрібен повний recoverability contract: backup потрібних extents або відновлюваний sidecar/launcher repair, atomic manifest, identity/size/hash checks, rollback test на реальному файлі.

### P1 — malformed action JSON перетворюється на `DirectDelete`

`crates/core/src/models.rs:52-59` на помилку deserialization повертає `FindingAction::DirectDelete`. Для category `MonolithicArchive` це робить пошкоджений/новіший DB action звичайним видаленням усього контейнера.

Парсинг destructive action має бути fail-closed: invalid/unknown action => blocked finding з поясненням, ніколи не `DirectDelete`.

### P1 — cleanup обходить наявний DeletePlan/identity safety

`crates/core/src/cleanup.rs:203-255` перевіряє існування path, створює snapshot і повторно відкриває шлях без використання вже наявного `SafetySnapshot`, containment та file-identity validation. Між перевіркою й відкриттям можливий swap/junction TOCTOU. Monolithic execution має повторно використати сильні інваріанти `DeletePlan`, працювати через перевірений handle і звіряти generation/action/ranges.

### P1 — CLI anti-cheat policy не є fail-closed

- Single-file `archive-trimmer trim` у `crates/archive_trimmer/src/cli.rs:410-431` одразу викликає handler; anti-cheat check відсутній.
- Batch/core дозволяють `force_unsafe` bypass: `crates/archive_trimmer/src/scanner.rs:129-133`, `crates/core/src/cleanup.rs:173-201`.
- Walk errors ігноруються: `crates/archive_trimmer/src/anti_cheat.rs:88-94`.
- Shallow reload check повертає safe при `read_dir` error: `crates/archive_trimmer/src/anti_cheat.rs:298-307`.
- `ValveAntiCheat` є в enum, але реального VAC detection/report path немає.

Це прямо суперечить формулюванню GT-146 «примусово блокується».

## Відповідність тікетам GT-140…146

| Тікет | Оцінка поточного стану | Основний розрив |
|---|---|---|
| GT-140 Sparse Core | Частково реалізовано | FSCTL wrappers є; 64 KiB snapshot не є rollback; acceptance test не zeroes заявлені 100 MB і не доводить звільнення allocation |
| GT-141 Wwise PCK/BNK | Частково / небезпечно | PCK — synthetic happy path; unknown IDs destructive; `BKHD` detector спрямовує BNK у parser, який вимагає `AKPK` (`formats/mod.rs:474-477,563-566`, `wwise.rs:227-235`) |
| GT-142 Unreal/repak | Не реалізовано за DoD | `repak` відсутній у dependencies; є спрощений handwritten parser і raw zeroing; loose/repack modes відсутні |
| GT-143 RE Engine | Heuristic spike | Перевіряються максимум 256 table entries, fallback лише перші 2 MiB (`re_engine.rs:203-285`); loose-loader відсутній |
| GT-144 Unity + ASAR | Не реалізовано за DoD | Unity `trim` — snapshot-only no-op з 0 savings (`unity.rs:58-114`); ASAR лише sparse-zero, без unpack до `app/` |
| GT-145 Bink | Непідтверджений prototype | 64 zero bytes оголошені compressed frame (`bink.rs:494-531`); тест читає результат тим самим permissive parser, RAD/BinkOpen не запускається |
| GT-146 Anti-Cheat | Не виконує fail-closed DoD | Verdict губиться в Phase 3; CLI bypass; traversal errors fail-open; VAC фактично не детектується |

### Додаткові parser risks

- Wwise table loops обмежені всім файлом та arbitrary caps, а не declared section bounds; parsed `header_size/version` не використовуються (`wwise.rs:238-242,253-294,299-380`). Corrupt descriptors можуть створити header-overlapping ranges.
- UE synthetic fixture пише голі payloads (`ue_pak.rs:432-445`), тому не моделює справжній serialized FPakEntry перед payload. Test не доводить безпечність offset semantics на реальному UE PAK.
- UE paths з `localization/.locres/.locmeta`, але без визначеної мови, все одно `is_language=true` (`ue_pak.rs:346-354`), після чого `unknown` потрапляє у destructive offsets.
- RE fallback приймає будь-які `AKPK` bytes у перших 2 MiB і водночас пропускає валідні блоки далі в 50 GB container.

## GT-376 — баги ідентифікації мов

Screenshots підтверджують щонайменше два класи проблем:

1. `pt-BR` та `zh-Hans` у UE paths показані як `[other]`.
   `extract_language_from_path` у `crates/archive_trimmer/src/formats/ue_pak.rs:380-423` не має цих variants.
2. Невідомі/частково розібрані Wwise entries отримують guessed/`other` labels і все одно можуть бути destructive targets.

Архівний модуль має окремий hard-coded registry у `formats/mod.rs:156-300`, який дрейфує від `l10n_rules.json` та основного `LangDetector`. Не вистачає, зокрема, коректного збереження `pt-br`, `zh-hans`, `zh-hant` і повного набору supported locales.

Рекомендація: один canonical language registry для звичайного scan і archive parsers; зберігати regional tags; unknown/ambiguous => `needs_review`, `can_trim=false`.

## GT-375 — причини повільного UI

Скріншот показує приблизно 721k findings. На кожному repaint UI робить кілька O(N) проходів:

- `tree_view.rs:252,394` — fingerprint усіх findings двічі;
- `plan_panel.rs:43,51` + `model.rs:605-638,656-671` — окремо перебудовує category cards і totals;
- `tree_view.rs:308-328` — `build_visible_rows` викликається двічі;
- `tree_view.rs:765-825` — відкритий моноліт матеріалізує `VisibleRow` для кожного stream до того, як `show_rows` virtualizes widgets.

Тому virtualization не усуває hashing/aggregation/allocation до render. Один великий Wwise container може породжувати сотні тисяч row pushes двічі за frame.

### Оптимізації

1. Кешувати plan summary/cards і invalidation лише при заміні findings, delete result або зміні category.
2. Замінити 2× fingerprint на `selection_dirty`/selection generation, який змінюють усі selection entry points.
3. Кешувати visible-row plan за `(tree_generation, toggle_generation, filter, search_generation)`; не будувати його двічі.
4. Не матеріалізувати всі streams у `Vec<VisibleRow>`: child range + paging/limit/«показати ще», окремий virtual list.
5. Не дублювати повні `all_indices` на category/game/top levels для 700k rows; зберігати ranges або aggregate metadata.
6. Кешувати normalized/case-folded path і sort keys; не створювати lowercase/owned keys під час regroup/sort.
7. Не показувати stream-level entries за замовчуванням для великих containers; summary за language/count/bytes достатній до explicit drill-down.

Keyboard також обходить disabled anti-cheat checkbox: `tree_view.rs:985-1001` перевіряє лише `deletion_block_reason`, тоді як checkbox має додатковий anti-cheat predicate (`:1976-1986`). Batch validation пізніше відмовить, але selection totals і profile стають неправдивими.

## Нечесний/неточний облік savings

- Final offsets фільтруються за user `keep_languages`, але `core/worker.rs:122-127` бере повний `analysis.estimated_savings_bytes`, порахований handler-ом для default English keep-list.
- Phase 3 записує цей estimate як `FindingRow.size_on_disk`: `app/worker/scan.rs:912-937`.
- Dry-run handlers віднімають повні logical lengths, хоча sparse hole punching використовує лише cluster-aligned interior ranges.
- `core/cleanup.rs:298-306` повертає `logical_zeroed` як `bytes_freed`, якщо measured physical delta дорівнює нулю.

Внаслідок цього badge, selected totals і confirmation можуть показувати байти, які не будуть звільнені. Savings потрібно рахувати із фінального набору validated, non-overlapping, cluster-alignable ranges; live result — лише з фактичної allocation delta, без logical fallback.

## 3-phase progress UI

Production scan worker надсилає `ScanPhase1` на старті гри (`app/worker/scan.rs:1322-1335`) і `ScanPhase3` (`:737-744,949-957`), але не надсилає `ScanPhase2`. Заявлені три окремі progress bars не мають повного event contract.

## Перевірки

| Команда | Результат |
|---|---|
| `cargo test -p archive_trimmer --lib` | PASS: 45/45 |
| `cargo test --workspace --all-features` | PASS: усі виконані suites; machine-dependent tests ignored |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | **FAIL**: багато EP10-файлів не відформатовано |
| `git diff --check` | **FAIL**: trailing blank line у `crates/core/src/settings.rs` |

Зелені unit tests не є production acceptance: destructive format tests використовують self-generated synthetic fixtures; відсутні app action-routing integration test, real PCK/BNK/UE/RE/ASAR/Unity corpus fixtures, external RAD/BinkOpen verification та game-launch checks.

## Рекомендований порядок виправлень

1. Негайно заблокувати selection/execution усіх monolithic findings у main GUI.
2. Додати action-aware, identity-safe execution через verified handles; invalid action має блокуватися.
3. Зробити language/format ambiguity fail-closed і перенести full-scan anti-cheat verdict у Phase 3.
4. Спроєктувати реальний rollback/repair contract та partial-failure recovery.
5. Розділити prototype handlers від supported handlers; не створювати destructive action, доки format-specific gates не пройдені.
6. Додати real-fixture + external-decoder + GUI end-to-end tests, після чого повторно переглянути Done-статуси GT-140…146.
7. Кешувати O(N) UI aggregates/visible rows і пагінувати streams.
8. Після correctness/safety виправити formatting gates та повторити повний suite.

## Readiness gates

До зняття NO-GO повинні одночасно пройти:

- main GUI integration test доводить, що monolith action ніколи не потрапляє у whole-file remover;
- unknown/ambiguous languages та unsupported formats залишаються read-only;
- anti-cheat errors/missing evidence fail closed у fresh scan, reload, GUI, single CLI і batch CLI;
- rollback повертає modified payload або офіційний repair path підтверджено end-to-end;
- real archive fixtures проходять parse/range validation, non-overlap та keep-language checks;
- Bink stub відкривається незалежним decoder-ом; UE path перевірено через `repak`/real engine tooling;
- live allocated-space delta, а не logical estimate, визначає фактичний результат;
- UI має виміряний responsive budget на dataset масштабу 700k findings;
- `test`, `clippy`, `fmt` і `diff --check` зелені.

## Додаток: стан після remediation і backlog завершення

Після незалежного remediation-review початкові production P0/P1 шляхи
whole-file deletion закрито. Архівні findings зараз навмисно read-only:
inspection і UI доступні, але Wwise/BNK/UE/RE/ASAR/Unity/Bink mutation
заблокована. Поточний стан є придатним як safety increment, але початкова мета
епіку — безпечне фізичне звільнення місця всередині монолітів — ще не виконана.

### Verification build

- Файл: `GameTrimmer-1.0.0-EP10-test-20260820-131645-win64.zip`.
- SHA-256: `33839E3EA32FD5D246004FCEE141B7FF7119B69682B50BA993240B4A98FEC857`.
- Source: `feat/epic-10-monolithic-archives`, commit
  `af0604b65172af35f529ccbf4f3775c8d76f60b6`, `source_dirty=true`.
- Manifest/PE/metadata та контрольований GUI startup перевірено незалежно.
- Це лише unsigned verification build для ручного тестування, не clean release.

### Backlog до feature-complete

1. **Stabilization поточного build.** Кожен ручний дефект оформити з точним
   reproduction, severity, game/archive evidence та regression-тестом.
2. **Повний recoverability contract.** Backup усіх modified extents, atomic
   manifest, path/identity/size/hash verification, free-space preflight,
   rollback і startup reconciliation після crash/full-disk/partial write.
3. **Окремі validated executors.** Не використовувати універсальний
   `SparseZero`: Wwise PCK, BNK, UE PAK, RE Engine, ASAR/Unity і Bink повинні
   мати окремі parse/plan/mutate/validate/rollback контракти; unsupported і
   ambiguous залишаються read-only.
4. **Real-format corpus.** Реальні архіви різних версій, malformed fixtures,
   independent parser/decoder validation, launcher verify/repair і запуск ігор
   до та після mutation.
5. **Mutation integration.** Action-aware journaled executor, повний batch
   preflight до першого запису, safe cancellation points, feature flag і
   заборона будь-якого fallback до whole-file delete.
6. **Anti-cheat acceptance.** Steam/launcher metadata для VAC та реальні
   EAC/BattlEye/Vanguard/VAC titles; однаковий fail-closed verdict у GUI, CLI,
   batch, reload і retrim безпосередньо перед mutation.
7. **Чесний space accounting.** Окремо logical candidate bytes, physical
   allocation before/after, backup overhead і net reclaimed bytes; dry-run не
   повинен подавати logical length як гарантоване фізичне звільнення.
8. **Scale і API hardening.** Large-library CPU/RAM/frame benchmarks, cache
   flattened visible rows, вирівняти або закрити secondary `core::worker`,
   додати anti-cheat gate до dormant standalone trim route.
9. **Windows/manual acceptance.** Recycle Bin quota, locked/read-only file,
   NTFS/ReFS, interruption, sleep/restart, full disk, portable mode, UAC,
   SmartScreen, DPI/locales і launcher recovery.
10. **Clean release candidate.** Усі source-файли tracked, clean commit,
    `source_dirty=false`, повні automated gates, SHA256SUMS, бажано підписані
    EXE, незалежна package verification і ручне приймання користувачем.

GT-140…GT-146 та GT-EP10 мають залишатися open до проходження відповідних
format-specific і release gates. GT-375/GT-376 залишаються окремими backlog
задачами для performance та language ambiguity.
