# GT-EP10 — звіт про remediation

Дата: 2026-08-20  
Гілка: `feat/epic-10-monolithic-archives`  
HEAD: `af0604b65172af35f529ccbf4f3775c8d76f60b6` + незакомічений робочий стан  
Базовий аудит: `docs/gt-ep10-implementation-audit-2026-08-20.md`

## Підсумок

Початкові P0/P1 шляхи втрати даних закрито. Production GUI, headless CLI,
automatic retrim та публічний core delete pipeline більше не можуть перетворити
монолітний контейнер на звичайне whole-file видалення через stale UI, пошкоджений
JSON, підмінену категорію правила, неоднозначне ім'я або magic bytes під
нестандартним розширенням.

Поточний режим EP10 навмисно **read-only**: інспекція та UI-представлення
архівів доступні, а будь-яка мутація Wwise/BNK/UE/RE/ASAR/Unity/Bink повертає
`Unsupported` або показується як заблокований finding. Це safe-to-merge як
safety increment, але **не feature-complete і не release-ready**.

Фінальний вердикт: **HOLD для релізу та закриття епіку**.

## Що виправлено

### Єдина fail-closed межа видалення

- GUI та headless CLI переносять точний `FindingAction` у delete request.
- `FindingAction` більше не перетворює malformed/non-empty JSON на
  `DirectDelete`.
- `prepare_delete_plans` є центральним authoritative preflight і перевіряє:
  активну scan generation, safety evidence, усі persisted category/action
  contracts, дублікати ID, archive-like path, live archive magic/type та
  filesystem identity.
- Увесь batch повторно перевіряється до першої мутації. Mixed/stale/corrupt
  batch відхиляється цілком.
- `DeletePlan` непрозорий; raw remover, verified target/open/delete та старий
  cleanup executor більше не є публічними destructive API.
- Operation intent/final outcome проходять через один journaled executor.

Регресійні сценарії покривають `monolithic_archive + NULL action`, stale UI
downgrade, ordinary-category smuggling, `AKPK` під назвами `manual.txt` і
`sounds_fra.pck`, mixed batch та identity change.

### Conservative archive policy

- `can_zero_in_place = false`, compressed/encrypted UE entries, Bink та
  невідома мова не утворюють executable range.
- Невідомі Wwise IDs та UE localization без надійного language tag обробляються
  fail-closed.
- Фінальні ranges перевіряються на межі файла й перетини; savings рахуються з
  фактично прийнятого набору, а не з початкової глобальної оцінки.
- BNK не передається AKPK parser-у; unsupported handlers повертають явний
  `Unsupported`.
- Standalone CLI/GUI mutation, force-unsafe, sparse writers і header restore
  прибрані з production API. Header snapshot/repair лишився тільки test-only і
  більше не подається як rollback.

### Rule smuggling, scan та retrim

- Ordinary/custom rules не можуть зарезервувати category
  `MonolithicArchive` або whole-delete supported container.
- Archive candidate отримує видимий non-selectable read-only placeholder.
- Imported/archive-like findings проходять bounded content probe; magic-only
  container не стає `DirectDelete`.
- Phase 3 транзакційно замінює placeholder деталізованою archive action без
  дубльованих DB/UI rows; unsupported analysis лишає blocked row із нульовою
  економією.
- Automatic retrim відхиляє весь run при `ImportedUntrusted` match і виконує
  дозволені whole-file операції тільки через центральний delete pipeline.

### Anti-cheat та fail-closed I/O

- Full-depth traversal не слідує symlink-ам; traversal/read/missing-directory
  errors трактуються як unsafe.
- Phase 3 визначає anti-cheat з повного live inventory candidate-game, кешує
  verdict один раз на гру та використовує `missing = protected`.
- Single/batch CLI не мають force bypass; додано VAC filename signatures.
- VAC coverage поки best-effort: launcher/Steam protected-app metadata ще
  немає, тому це залишається release gate для майбутньої mutation.

### UI, load та accounting

- Mouse, keyboard, profile, select-all, group checkbox і stream rows
  використовують один eligibility predicate; non-`DirectDelete` не можна
  вибрати.
- Stream detail пагінується по 128; прибрано подвійні O(N) selection
  fingerprints і звичайний подвійний visible-row build.
- Bottom bar і plan panel використовують один `UiAggregates` snapshot на frame.
- Reload кешує anti-cheat один раз на relevant game, відновлює monolith badge і
  обмежує reclaimable estimate logical/physical розміром.
- Негативні SQLite sizes пропускаються з diagnostic; byte aggregates і
  fallback sums saturating, без panic/wrap.
- Phase 2 має монотонний game-level progress contract.
- EN/UK та locale JSON чесно описують read-only inspection і відсутність safe
  trimming/rollback.

## Перевірки

| Gate | Результат |
|---|---|
| `cargo test --workspace --all-features` | PASS |
| App unit tests | 531 passed, 2 ignored |
| Core unit tests | 630 passed, 2 ignored |
| `archive_trimmer` unit tests | 52 passed |
| Core corpus/l10n/report/Recycling integration | PASS; quota probe ignored як machine-dependent |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo fmt --all -- --check` | PASS |
| `git diff --check` | PASS; лише інформаційні CRLF warnings |
| `cargo audit --deny warnings` | PASS; 481 lockfile dependencies scanned |
| Незалежний adversarial quality suite | 34/34 targeted scenarios PASS |

## Свідомі safety-регресії

До появи окремого trusted whole-file exception contract блокуються всі
розпізнані supported containers, зокрема колишнє whole-file видалення external
language `.pck/.pak` і Bink intro `.bik/.bk2`. Це навмисна втрата capability,
щоб жодне правило або неоднозначне ім'я не видалило багатомовний контейнер.

## Що лишається відкритим

- Немає production payload backup/restore та transactional rollback для
  sparse/repack/stub mutation.
- Реальні Wwise/BNK/UE/RE/ASAR/Unity/Bink corpus fixtures, незалежні decoder
  checks, launcher verify/repair і game-launch acceptance відсутні.
- VAC detection є filename heuristic, не launcher/Steam metadata verdict.
- Delete batch atomic до mutation, але runtime failure після початку може дати
  partial outcome; journal фіксує стан, однак permanent delete не відновлюється.
- Фактичне cluster allocation reclaim не вимірюється, бо mutation вимкнена.
- Повністю розгорнуте велике дерево досі будує/алокує `VisibleRow` для всіх
  структурно видимих rows щокадру.
- Secondary `core::worker` API має застарілий Phase2/Phase3/provenance contract;
  production app його не використовує для destructive pipeline, а центральний
  preflight блокує його неповні rows. API слід private/deprecate або вирівняти.
- Standalone GUI worker має dormant trim route без власного anti-cheat gate;
  handlers і кнопки зараз заблоковані. Gate обов'язковий перед увімкненням
  будь-якого handler.

## Release gates

Епік не можна закривати, доки одночасно не підтверджено:

1. Повний відновлюваний backup/rollback і crash/full-disk reconciliation для
   кожного executable archive action.
2. Real-format corpus + independent parser/decoder validation.
3. Launcher verify/repair та game-launch tests на реальних іграх.
4. EAC/BattlEye/VAC acceptance без bypass.
5. Recycle Bin quota/recovery та interruption tests.
6. Large-library scan/UI benchmark і portable GUI smoke.

До цього моменту GT-140…GT-146 мають залишатися незавершеними, а EP10 — open.

## Verification build після remediation

Новий пакет для ручного тестування створено з поточного integrated worktree:

| Поле | Значення |
|---|---|
| ZIP | `GameTrimmer-1.0.0-EP10-test-20260820-131645-win64.zip` |
| Розмір | 7 575 410 bytes |
| SHA-256 | `33839E3EA32FD5D246004FCEE141B7FF7119B69682B50BA993240B4A98FEC857` |
| Branch | `feat/epic-10-monolithic-archives` |
| Commit | `af0604b65172af35f529ccbf4f3775c8d76f60b6` |
| Build metadata | `source_dirty=true`, Rust 1.97.0 |
| Незалежний package gate | PASS |

ZIP містить 39 entries, обидва AMD64 PE32+ GUI executables, 30 locale JSON та
не містить окремих PDB/DB/log/debug-artifacts. Контрольований запуск із чистої
portable temp-теки створив вікно `GameTrimmer` і залишався стабільним. EXE не
підписані; локальні CodeView paths і dirty source роблять цей пакет придатним
лише для verification/manual acceptance, не для публічного релізу.

## Execution backlog до закриття епіку

### Milestone 1 — stabilization read-only build

- Зібрати всі знайдені під час ручного тестування проблеми з reproduction,
  expected/actual behavior, severity, грою/архівом, логом або screenshot.
- На кожен підтверджений дефект спочатку додати regression-тест, потім fix.
- Повторити automated suite і видати новий timestamped verification build.

**DoD:** усі P0/P1 дефекти поточного inspection/UI workflow закриті, ручний
read-only smoke пройдено, жоден archive finding не може бути виконаний як
whole-file delete.

### Milestone 2 — recoverability framework

- Backup усіх extents, які mutation змінить, а не лише header.
- Atomic sidecar manifest із format/action, normalized path, filesystem
  identity, original size/hash, ranges і станом операції.
- Free-space preflight, durable writes, resume/rollback і reconciliation після
  crash, cancellation, full disk та restart.
- Fault-injection tests на кожній межі запису.

**DoD:** після штучного падіння на будь-якому кроці файл або не змінено, або
повністю відновлено з перевіреним hash; незавершена операція не губиться.

### Milestone 3 — format-specific mutation

- **GT-141 Wwise:** validated PCK ranges; unknown IDs read-only; окремий
  справжній BNK parser або explicit unsupported.
- **GT-142 UE:** production repack для supported versions; compressed entries
  ніколи не zero-in-place.
- **GT-143 RE Engine:** versioned full resource-table traversal, без обмеженого
  raw-signature scan як production coverage.
- **GT-144 ASAR/Unity:** validated unpack/repack та незалежне відкриття output.
- **GT-145 Bink:** format-specific stub, який приймає незалежний RAD/Bink
  decoder; BIK/BK2 не вважаються взаємозамінними без доказу.

Для кожного enabled handler потрібен ланцюжок parse → plan → backup → mutate →
independent validate → rollback. Формат без повного ланцюжка лишається
read-only і не створює executable action.

### Milestone 4 — real-world safety acceptance

- Real archive corpus різних ігор/версій плюс malformed/truncated cases.
- Independent parser/decoder checks, launcher verify/repair і game launch до та
  після mutation/rollback.
- EAC, BattlEye, Vanguard і VAC titles; VAC verdict доповнити Steam/launcher
  metadata, а не покладатися лише на filenames.
- Однаковий live fail-closed gate для fresh scan, reload, GUI, CLI, batch і
  retrim безпосередньо перед mutation.

**DoD:** supported matrix має зафіксовані title/build/format/version результати;
unknown build або неповний anti-cheat verdict автоматично блокує mutation.

### Milestone 5 — accounting, scale і API convergence

- Показувати окремо logical removable bytes, allocation before/after, backup
  overhead і net reclaimed bytes; прибрати logical fallback із success result.
- Benchmark scan/UI/DB/RAM на великій бібліотеці й 500k+ streams; кешувати
  flattened visible rows для повністю розгорнутих дерев.
- Private/deprecate або вирівняти secondary `core::worker` Phase2/Phase3 API.
- Додати anti-cheat preflight до dormant standalone GUI route до ввімкнення
  будь-якого handler.

### Milestone 6 — Windows і clean release candidate

- Recycle Bin quota/recovery, locked/read-only file, NTFS/ReFS, sparse support,
  sleep/restart, cancellation, crash, full disk і portable writable-path tests.
- Повний manual UI plan: UAC/SmartScreen, DPI, locales, велика бібліотека,
  launcher recovery та post-operation summaries.
- Усі потрібні source-файли tracked; clean commit і clean worktree.
- Повторити tests/clippy/fmt/diff/audit, зібрати `source_dirty=false` package,
  створити SHA256SUMS, перевірити package незалежно; підписати EXE перед
  public release, якщо доступна release-signing процедура.
- Отримати явне ручне acceptance користувача, і лише після цього оновлювати
  GT-140…GT-146 та GT-EP10 до Done.

Порядок виконання: спочатку дефекти поточного тестового build, потім стабільний
read-only increment, recoverability framework, формати по одному, real-game
acceptance і лише в кінці clean release candidate.
