# Механізми відновлення файлів гри в лаунчерах (спайк для GT-01)

Дата дослідження: **2026-07-24**. Статус: **кабінетна частина завершена, польова —
не проводилась** (жодної команди на реальних клієнтах не запускали, див. «План
емпіричної перевірки»).

Питання картки GT-01: для кожного з провайдерів, які підтримує GameTrimmer, чи є
спосіб **ззовні** запустити штатну перевірку/відновлення цілісності конкретної
гри, і чи маємо ми ідентифікатор, якого цей спосіб вимагає.

## Джерела доказів

Три різні за надійністю рівні — у зведеній таблиці позначені саме вони, а не
«працює/не працює»:

- **A. Бінарник** — рядки, витягнуті з локально встановлених клієнтів
  (ASCII + UTF-16LE). Найнадійніше з доступного без запуску: показує, що
  механізм у клієнті **є** і як він усередині зветься, але не доводить, що він
  досяжний ззовні.
- **B. Реєстр** — зареєстровані URL-протоколи на цій машині (`HKCR`).
- **C. Веб** — офіційна довідка вендора, спільнотні реверси, суміжні
  open-source проєкти.

На машині розробника зареєстровані протоколи: `steam`, `com.epicgames.launcher`,
`uplay`, `goggalaxy`, `rockstar`. Не зареєстровані (клієнти не встановлені):
`battlenet`, `origin`/`origin2`/`link2ea`, `riotclient`, `amazon-games`, `itch`.

## Зведення

| Провайдер | Механізм ззовні | Рівень доказу | Наш `app_id` підходить? | Придатність для GT-01 |
|---|---|---|---|---|
| steam | `steam://validate/<appid>` | C (широко задокументовано) | так — числовий appid із `appmanifest_*.acf` | **Так** |
| epic | `com.epicgames.launcher://apps/<id>?action=…` | A (сильний) + B | ймовірно — `AppName` з `.item`-маніфесту | **Так, після перевірки токена** |
| gog | `GalaxyClient.exe /command=…` або `goggalaxy://…` | A (механізм є, зовнішній вхід не підтверджено) | так — числовий `gameId` | **Кандидат, потребує тесту** |
| ubisoft | не знайдено | A (негативний) + C | — | Ні — інструкція |
| rockstar | не знайдено | A (негативний) | у нас `app_id: None` | Ні — інструкція |
| battlenet | Agent REST API (localhost:1120) | C | **ні** — потрібен TACT-код, у нас ключ реєстру | Ні — інструкція |
| ea | не знайдено | C | — | Ні — інструкція |
| amazon, itch, humble, riot, xbox | не знайдено | C | — | Ні — інструкція |

## Steam

`steam://validate/<appid>` — відкриває Steam і запускає «Перевірити цілісність
файлів гри» для вказаного застосунку. Ідентифікатор у нас уже є:
`parse_appmanifest` кладе `appid` у `GameInstall.app_id`
([steam.rs:171](../../crates/core/src/providers/steam.rs:171)).

Важлива межа, яку картка вже фіксує: валідація повертає **всі** відсутні файли
гри, а не вибірково зачищені GameTrimmer. Для нашого наративу це саме те, що
треба, але в UI має бути сказано явно.

## Epic Games Launcher — механізм точно існує

Найцінніша знахідка спайку. У `EpicGamesLauncher.exe` (рівень A) присутні:

```
com.epicgames.launcher://apps/
&action=%s
?action=launch&silent=true
AppVerifyURICommand
AppVerifyURICommandNotification
AppVerifyURICommand_VerifyQueued
AppVerifyUriHandler
CanHandleVerify
App %s is busy or running, deferring verify to %s.
App %s not installed, skipping remote verify.
```

Тобто в лаунчері є **окремий URI-обробник верифікації** (`AppVerifyUriHandler`),
з чергою (`VerifyQueued`), сповіщенням і навіть обробкою випадку «гра зараз
запущена». Це знімає головне «невідомо» картки GT-01: механізм у Epic є і
викликається саме через URI, а не лише з UI.

Чого бінарник **не** доводить: точного значення `action=`. Формат URI
підтверджений (`apps/<id>?action=<verb>`), сам токен (`verify`) треба підтвердити
запуском — це один із пунктів польової перевірки нижче.

Ідентифікатор: наш `app_id` для Epic — це `AppName` з `.item`-маніфесту
([epic.rs:93](../../crates/core/src/providers/epic.rs:93)), і саме `AppName`
використовується в коротких Epic-ярликах. Але трапляється й довга форма
`<namespace>:<catalogItemId>:<artifactId>` — чи приймає обробник коротку для
будь-якої гри, теж перевіряємо польово.

## GOG Galaxy — механізм є, зовнішній вхід не доведений

У `GalaxyClient.exe` (рівень A) є `GogVerifyGameAction`, рядок
`Handling GogVerifyGameAction by GogActionReceiver.` і токен `VerifyGame` — поруч
із рештою родини дій (`GogInstallGameAction`, `GogUpdateGameAction`,
`GogUninstallGameAction`, `GogLaunchGameAction`).

Водночас зовнішній інтерфейс, який видно в тому ж бінарнику, — це
`/command=<verb>` та `/urlProtocol="%1"` (реєстр підтверджує другий), і серед
логів команд трапляються лише `installGame`, `updateGame`, `rungame`,
`installed`, `installedDLC`, `installationScreen`, `gamepatchedexternally`,
`openGameView`. **`verifyGame` серед них не видно** — схоже, що verify живе на
внутрішній шині «UI → нативна частина», а не у зовнішньому парсері команд.

Кандидати для перевірки (по спаданню ймовірності):
`GalaxyClient.exe /command=verifyGame /gameId=<id>` ·
`goggalaxy://verifyGame/<id>` · `goggalaxy://openGameView/<id>` (гарантований
фолбек: відкриває сторінку гри, далі користувач тисне «Verify / Repair» сам).

## Ubisoft Connect, Rockstar — негативний результат

**Ubisoft.** Ані `UbisoftConnect.exe`, ані `upc.exe` не містять рядків
`uplay://…` з дієсловами чи будь-чого на кшталт `verifyfiles`. UI лаунчера — на
CEF (`libcef.dll`, `*.pak`), тож логіка верифікації, найімовірніше, у
веб-ресурсах і назовні не виставлена. Спільнота припускала
`UbisoftConnect.exe /verifyfiles <id>` та `ubisoft://verifyfiles/<id>`, але
підтвердження не було.

**Rockstar.** У `Launcher.exe` уся машинерія верифікації присутня
(`Verify requested for title %s`, `Verify requested without a title provided!`,
`CancelVerify`, `VerifyTamperedFiles`), є прапорець `-titleid`, але
прапорця з `verify`/`repair` немає, а серед URI лише `rockstar://settings` і
`rockstar://social`. Плюс у нас для Rockstar `app_id: None`
([rockstar.rs:68](../../crates/core/src/providers/rockstar.rs:68)) — навіть за
наявності механізму нам не було б чим його адресувати.

## Battle.net — технічно можливо, але не за ціну картки

Repair у Blizzard виконує не лаунчер, а `Agent.exe` — він тримає локальний REST
API (типово `localhost:1120`): запит на `/agent` без авторизації повертає токен,
далі всі виклики йдуть із заголовком `Authorization`. Саме так працює
сторонній BNetInstaller (`--prod <TACT-код> --repair`).

Два стопери для GT-01: (1) це недокументований приватний API з токеном і
портом, що змінюється, — на порядок дорожче за «відкрити URL»; (2) наш `app_id`
для Battle.net — це ім'я ключа в реєстрі деінсталяції (наприклад `Overwatch`,
[battlenet.rs:123](../../crates/core/src/providers/battlenet.rs:123)), а API
потребує TACT-коду продукту (`pro`, `wow`, `s2`) — потрібна окрема таблиця
відповідності. Рекомендація: **не робити в межах GT-01**, лишити інструкцію.

## EA, Amazon, itch, Humble, Riot, Xbox

Жодного підтвердженого зовнішнього входу «перевір цю гру»:

- **EA app** — офіційний шлях лише через UI (три крапки на плитці гри → Repair).
- **Amazon Games** — repair лише в UI.
- **itch** — поняття «верифікація» відсутнє як таке.
- **Humble** — ігри не керуються лаунчером; наш `app_id` — `machine_name`.
- **Riot** — «Full Repair Tool» лагодить сам клієнт, не окрему гру.
- **Xbox / MS Store** — ремонт на рівні пакета застосунку (Параметри → Додатки →
  Додаткові параметри → Відновити), не per-game і не за нашим ідентифікатором.

## Що це означає для реалізації GT-01

1. **Три рівні поведінки пункту меню, а не два.** Steam і Epic — прямий запуск;
   GOG — запуск за результатом польової перевірки, інакше «відкрити сторінку
   гри» + підказка; решта — пояснення з кроками для конкретного вендора.
   Ховати пункт мовчки не можна (це вимога самої картки).
2. **Відсутній `app_id` — окремий випадок, не помилка.** Ігри, знайдені через
   `folderscan`, майже завжди мають `app_id: None`
   ([folderscan.rs:140](../../crates/core/src/providers/folderscan.rs:140), виняток —
   GOG із `goggame-*.info`). Тобто навіть для гри з `vendor = "steam"`
   ідентифікатора може не бути, і UI має падати в інструкцію.
3. **Оцінка картки (S) залишається чинною** — за умови, що Battle.net Agent API
   до неї не входить, а GOG деградує до `openGameView`.
4. **Обсяг верифікації треба назвати чесно**: жоден із цих механізмів не
   відновлює «те, що зняв GameTrimmer», — вони повертають гру до повного складу.

## План емпіричної перевірки (не виконано)

Робити на цій машині — тут є Steam, Epic, Ubisoft Connect, GOG Galaxy, Rockstar.
Кожен пункт **тягне реальне докачування файлів**, тому виконувати свідомо і на
дешевій за розміром грі.

| # | Що перевіряємо | Команда | Очікуване |
|---|---|---|---|
| 1 | Steam: базовий шлях | `steam://validate/<appid>` | діалог перевірки цілісності для цієї гри |
| 2 | Epic: токен дії | `com.epicgames.launcher://apps/<AppName>?action=verify` | лаунчер стає у чергу верифікації |
| 3 | Epic: коротка форма id | те саме на грі, де `AppName` не збігається з довгою формою | працює / потрібна довга форма |
| 4 | Epic: гра запущена | п.2 під час запущеної гри | «deferring verify» замість тихої втрати |
| 5 | GOG: команда | `GalaxyClient.exe /command=verifyGame /gameId=<id>` | старт Verify/Repair |
| 6 | GOG: URI | `goggalaxy://verifyGame/<id>` | те саме |
| 7 | GOG: фолбек | `goggalaxy://openGameView/<id>` | відкрита сторінка гри |
| 8 | Ubisoft: контрольний | `uplay://verifyfiles/<id>` | очікуємо провал — фіксуємо як доказ |

Результати п.2, 5, 6 визначають, чи Epic і GOG потрапляють у «прямий запуск», чи
разом з рештою в «інструкцію».
