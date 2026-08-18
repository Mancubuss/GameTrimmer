import json
import subprocess
import urllib.request
import urllib.error
import sys

sys.stdout.reconfigure(encoding='utf-8')

# 1. Get decrypted token from DPAPI via powershell
ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\antigravity-agent.api-token.dpapi")
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""

res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True)
token = res.stdout.strip()
if not token:
    raise RuntimeError("Failed to obtain decrypted Vikunja API token.")

headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

# Clean up task 319 if present
try:
    req = urllib.request.Request("http://127.0.0.1:3456/api/v2/tasks/319", headers=headers, method="DELETE")
    urllib.request.urlopen(req)
    print("Deleted prior task 319.")
except Exception:
    pass

tasks = [
    {
        "title": "GT-EP11 · Комерціалізація, зміна ліцензії та дистрибуція у Steam і на інших платформах",
        "priority": 3,
        "labels": [{"id": 39}, {"id": 37}],
        "description": """<p><strong>ЕПІК: Комерціалізація, ліцензування та багатоплатформна дистрибуція GameTrimmer</strong></p>
<p><strong>СУТЬ:</strong> Комплексна стратегія та дорожня карта комерціалізації GameTrimmer, переходу на комерційну/подвійну модель ліцензування (збереження ліцензійної чистоти без copyleft-інфекцій) та релізу в Steam Direct, Epic Games Store, GOG, Microsoft Store та Direct-to-Consumer (D2C).</p>
<p><strong>СТРУКТУРА СПАЙКІВ:</strong></p>
<ul>
<li><strong>GT-147:</strong> [Спайк: Ліцензія] Аудит відкритого коду, залежностей та стратегія зміни ліцензії (MIT → Dual/Commercial/Open-Core).</li>
<li><strong>GT-148:</strong> [Спайк: Steam] Публікація утиліти в Steam: Steam Direct, правила Valve, інтеграція Steamworks та Steam Pipe.</li>
<li><strong>GT-149:</strong> [Спайк: Платформи] Дослідження альтернативних платформ дистрибуції (itch.io, Epic Games Store, GOG, Microsoft Store, D2C).</li>
<li><strong>GT-150:</strong> [Спайк: Юриспруденція & EULA] Юридичний комплаєнс, EULA, товарні знаки (Nominative Fair Use), GDPR та податкове структурування.</li>
<li><strong>GT-151:</strong> [Спайк: Реліз-інжиніринг] Підпис коду (Code Signing), захист ліцензійних ключів та система оновлень.</li>
</ul>"""
    },
    {
        "title": "GT-147 · [Спайк: Ліцензія] Аудит відкритого коду, залежностей та стратегія зміни ліцензії (MIT → Dual/Commercial/Open-Core)",
        "priority": 3,
        "labels": [{"id": 39}, {"id": 23}],
        "description": """<p><strong>СПАЙК: Зміна ліцензії, права авторів та аудит транзитивних залежностей</strong></p>
<p><strong>МЕТА:</strong> Визначити оптимальну правову модель переходу з поточної ліцензії MIT на комерційну форму монетизації та гарантувати 100% ліцензійну чистоту коду й транзитивних залежностей без copyleft-ризиків.</p>
<hr/>
<h3>Напрямки дослідження:</h3>
<ol>
<li><strong>Аудит залежностей (Cargo.lock):</strong>
<ul>
<li>Повна інвентаризація crates за допомогою <code>cargo-deny</code> / <code>cargo-license</code> на відповідність пермісивним ліцензіям (MIT, Apache-2.0, BSD-2/3, zlib).</li>
<li>Перевірка відсутності заборонених copyleft (GPL, AGPL, статично лінкований LGPL).</li>
<li>Статус бібліотек: <code>ntfs</code>, <code>rusqlite</code> (Public Domain SQLite), <code>windows-rs</code>, <code>eframe/egui</code>, <code>trash</code>, <code>zip</code>, <code>rayon</code>.</li>
</ul>
</li>
<li><strong>Правовий статус поточної кодової бази:</strong>
<ul>
<li>Оскільки 100% коду створено автором проекту GameTrimmer, автор володіє повним обсягом авторських і майнових прав і має право релізити майбутні версії під комерційною/пропрієтарною ліцензією без згоди третіх сторін.</li>
</ul>
</li>
<li><strong>Вибір моделі ліцензування:</strong>
<ul>
<li><em>Proprietary Commercial:</em> комерційна пропрієтарна ліцензія для платної версії у Steam та інших магазинах.</li>
<li><em>Dual-Licensing:</em> безкоштовна версія під GPLv3/AGPL для відкритих контриб'юцій + платна комерційна EULA для магазинів без обов'язку відкривати вихідний код релізних збірок.</li>
<li><em>Open-Core / Source-Available:</em> вільне ядро <code>gametrimmer-core</code> (MIT/BSL/FSL) + комерційні модулі GUI, Steamworks та просунутого тримінгу монолітів.</li>
</ul>
</li>
<li><strong>Захист контриб'юцій (CLA / DCO):</strong>
<ul>
<li>Впровадження Contributor License Agreement (CLA) або Developer Certificate of Origin (DCO) для гарантії збереження за автором виключного права на комерційну експлуатацію.</li>
</ul>
</li>
<li><strong>Third-Party Notices:</strong>
<ul>
<li>Оновлення <code>THIRD-PARTY-NOTICES.md</code> з дотриманням умов авторських прав усіх використаних компонентів (зокрема TikiOne Steam Cleaner notices).</li>
</ul>
</li>
</ol>
<hr/>
<ul data-type="taskList">
<li data-type="taskItem" data-checked="false"><p>Здійснити аудит усіх прямих і транзитивних crates у <code>Cargo.lock</code>.</p></li>
<li data-type="taskItem" data-checked="false"><p>Підготувати шаблон комерційної ліцензії / Commercial EULA для релізних білдів.</p></li>
<li data-type="taskItem" data-checked="false"><p>Скласти текст CLA / DCO для зовнішніх контриб'юторів у <code>CONTRIBUTING.md</code>.</p></li>
<li data-type="taskItem" data-checked="false"><p>Сформувати підсумковий звіт аудиту у <code>docs/</code>.</p></li>
</ul>"""
    },
    {
        "title": "GT-148 · [Спайк: Steam] Публікація утиліти в Steam: Steam Direct, правила Valve, інтеграція Steamworks та Steam Pipe",
        "priority": 3,
        "labels": [{"id": 39}, {"id": 23}],
        "description": """<p><strong>СПАЙК: Вихід у Steam Store: вимоги Valve, інтеграція Steamworks SDK та пайплайн Steam Pipe</strong></p>
<p><strong>МЕТА:</strong> Дослідити всі організаційні, фінансові, технічні та регуляторні кроки для успішної публікації GameTrimmer як платної утиліти в Steam Store.</p>
<hr/>
<h3>Напрямки дослідження:</h3>
<ol>
<li><strong>Steam Direct & Онбординг розробника:</strong>
<ul>
<li>Реєстрація у Steamworks ($100 Steam Direct Fee, повертається при досягненні $1000 виручки).</li>
<li>Податковий комплаєнс: заповнення W-8BEN / W-8BEN-E, підтвердження TIN (РНОКПП для України), застосування 0% або 10% ставки податку за договором про уникнення подвійного оподаткування.</li>
<li>Банківська інтеграція: валютний рахунок SWIFT/IBAN у USD/EUR для регулярних виплат від Valve.</li>
</ul>
</li>
<li><strong>Політики Valve щодо утиліт та модифікації ігор:</strong>
<ul>
<li>Категорія застосунку: <em>Software &rarr; Utilities / System Utilities</em>.</li>
<li>Аналіз Steam Subscriber Agreement (SSA) та Steam Distribution Agreement (SDA): легітимність очищення та оптимізації файлів у директорії <code>steamapps/common</code>.</li>
<li>Вимоги щодо безпеки: обов'язковість підтвердження дій користувачем, відсутність автодеструктивних дій, прозорість логування.</li>
</ul>
</li>
<li><strong>Інтеграція Steamworks API (<code>steamworks-rs</code>):</strong>
<ul>
<li>Дослідження моделі DRM: Steam DRM vs автономний DRM-free білд (чи дозволяти запуск без активного клієнта Steam).</li>
<li>Steam Cloud: синхронізація пресетів користувача та історії тримінгу.</li>
<li>Підтримка гілок оновлень (Steam Beta Branches: Default/Stable, Beta/Nightly).</li>
</ul>
</li>
<li><strong>Автоматизація збірок і релізів (Steam Pipe):</strong>
<ul>
<li>Конфігурація депотів (depots), скриптів <code>builder.exe</code> та інтеграція в GitHub Actions CI/CD для автодеплою релізів.</li>
</ul>
</li>
<li><strong>Маркетинг та сторінка магазину:</strong>
<ul>
<li>Вимоги до графічних капсул (Header Capsule 460x215, Small Capsule 231x87, Main Capsule 616x353), скріншотів, трейлера та локалізації опису.</li>
</ul>
</li>
</ol>
<hr/>
<ul data-type="taskList">
<li data-type="taskItem" data-checked="false"><p>Оформити чек-лист реєстрації та онбордингу в Steamworks.</p></li>
<li data-type="taskItem" data-checked="false"><p>Перевірити технічні вимоги для <code>steamworks-rs</code> у Rust 1.80+.</p></li>
<li data-type="taskItem" data-checked="false"><p>Підготувати шаблон скриптів деплою для Steam Pipe.</p></li>
<li data-type="taskItem" data-checked="false"><p>Скласти рекомендації щодо оформлення сторінки магазину та позиціонування утиліти.</p></li>
</ul>"""
    },
    {
        "title": "GT-149 · [Спайк: Платформи] Дослідження альтернативних платформ дистрибуції (itch.io, Epic Games Store, GOG, Microsoft Store, D2C)",
        "priority": 2,
        "labels": [{"id": 39}, {"id": 23}],
        "description": """<p><strong>СПАЙК: Аналіз додаткових платформ поширення та прямих продажів (D2C)</strong></p>
<p><strong>МЕТА:</strong> Оцінити ринковий потенціал, технічні вимоги, бар'єр входу та комісії альтернативних каналів дистрибуції для GameTrimmer.</p>
<hr/>
<h3>Аналізовані майданчики:</h3>
<ol>
<li><strong>itch.io:</strong>
<ul>
<li>Умови: $0 вхідний поріг, гнучка комісія від 0% до 100% (дефолт 10%), ідеально для інді-аудиторії та early adopters.</li>
<li>Інструменти: деплой білдів через Butler CLI, проста інтеграція.</li>
</ul>
</li>
<li><strong>Microsoft Store (Windows App Store):</strong>
<ul>
<li>Умови: $19 (фізособа) / $99 (компанія), комісія 15% (або 0% при власному білінгу для неігрових додатків).</li>
<li>Ключова перевага: автоматична повна довіра Windows SmartScreen без необхідності придбання дорожчих EV Code Signing сертифікатів.</li>
<li>Технологія: пакування в MSIX або звичайний Win32 App installer.</li>
</ul>
</li>
<li><strong>Epic Games Store (EGS):</strong>
<ul>
<li>Умови: $100 fee за гру/утиліту, розподіл 88/12, доступ до багатомільйонної аудиторії EGS.</li>
<li>Вимоги: Epic Online Services (EOS), модерація якості, IARC рейтинги.</li>
</ul>
</li>
<li><strong>GOG.com / GOG Galaxy:</strong>
<ul>
<li>Умови: кураторський відбір заявок, сувора вимога 100% DRM-Free білдів.</li>
<li>Аудиторія: велика база гравців у класичні ігри, які накопичують старі redistributables та гігабайти баласту.</li>
</ul>
</li>
<li><strong>Прямі продажі (D2C) через Merchant of Record (MoR):</strong>
<ul>
<li>Сервіси: Lemon Squeezy, Paddle, Gumroad, FastSpring.</li>
<li>Перевага: MoR бере на себе податкову відповідальність і сплату VAT/GST у 100+ країнах. Розробник отримує чистий дохід без податкової бюрократії.</li>
</ul>
</li>
</ol>
<hr/>
<ul data-type="taskList">
<li data-type="taskItem" data-checked="false"><p>Скласти порівняльну матрицю платформ (комісії, вартість входу, плюси/мінуси).</p></li>
<li data-type="taskItem" data-checked="false"><p>Визначити черговість релізів (Roadmap запуску за фазами).</p></li>
<li data-type="taskItem" data-checked="false"><p>Дослідити пакування Rust-бінарника в MSIX для Microsoft Store.</p></li>
</ul>"""
    },
    {
        "title": "GT-150 · [Спайк: Юриспруденція & EULA] Юридичний комплаєнс, EULA, товарні знаки та захист від ризиків",
        "priority": 3,
        "labels": [{"id": 39}, {"id": 23}],
        "description": """<p><strong>СПАЙК: Юридичний комплаєнс, EULA, товарні знаки (Nominative Fair Use) та мінімізація ризиків</strong></p>
<p><strong>МЕТА:</strong> Забезпечити повний юридичний захист автора та утиліти від претензій користувачів, правовласників ігор та розробників античит-систем.</p>
<hr/>
<h3>Напрямки аналізу:</h3>
<ol>
<li><strong>Складання EULA (End User License Agreement):</strong>
<ul>
<li>Чітко сформульований Disclaimer of Warranties та Limitation of Liability (програмне забезпечення надається «AS-IS»).</li>
<li>Зняття відповідальності за випадкову втрату даних, пошкодження файлів ігор або блокування акаунтів античитами (EAC, BattlEye, VAC).</li>
<li>Фіксація принципу «User-Driven Action»: утиліта лише пропонує та сканує, а остаточне рішення про видалення/модифікацію ухвалює користувач.</li>
</ul>
</li>
<li><strong>Товарні знаки та брендинг (Nominative Fair Use):</strong>
<ul>
<li>Правила використання назв лаунчерів (Steam, Epic Games, Ubisoft, EA, Riot, GOG) та назв ігор у UI та скріншотах: дотримання критеріїв номінативного добросовісного використання без порушення прав торговельних марок.</li>
<li>Обов'язковий текст Trademark Disclaimer в About та на сторінках магазинів.</li>
</ul>
</li>
<li><strong>Політика конфіденційності (Privacy Policy) & GDPR:</strong>
<ul>
<li>Підтвердження відсутності збору персональних даних (No PII).</li>
<li>Правила обробки локальних діагностичних пакетів (тільки локально, відправка лише за ініціативою користувача).</li>
</ul>
</li>
<li><strong>Організаційна та податкова форма:</strong>
<ul>
<li>ФОП 3-ї групи (Україна, 5% єдиного податку) + відповідні КВЕД (62.01, 58.29, 62.02) для легального отримання виплат від Valve Corporation (США) та інших іноземних платформ.</li>
</ul>
</li>
</ol>
<hr/>
<ul data-type="taskList">
<li data-type="taskItem" data-checked="false"><p>Підготувати шаблон EULA / Умов використання.</p></li>
<li data-type="taskItem" data-checked="false"><p>Підготувати текст Privacy Policy для сторінки в Steam/Web.</p></li>
<li data-type="taskItem" data-checked="false"><p>Сформулювати дисклеймери щодо товарних знаків для інтерфейсу та промо-матеріалів.</p></li>
</ul>"""
    },
    {
        "title": "GT-151 · [Спайк: Реліз-інжиніринг] Підпис коду (Code Signing), захист ліцензійних ключів та система оновлень",
        "priority": 2,
        "labels": [{"id": 39}, {"id": 23}, {"id": 37}],
        "description": """<p><strong>СПАЙК: Технічна архітектура комерційного релізу: Code Signing, офлайн-ліцензування та автооновлення</strong></p>
<p><strong>МЕТА:</strong> Спроєктувати та протестувати компоненти захисту бінарників, перевірки ліцензійних ключів та механізми доставки оновлень поза межами Steam.</p>
<hr/>
<h3>Напрямки розробки:</h3>
<ol>
<li><strong>Підпис коду (Code Signing):</strong>
<ul>
<li>Порівняння Standard OV Code Signing, EV Code Signing та Microsoft Azure Trusted Signing.</li>
<li>Інтеграція кроку підпису за допомогою <code>signtool</code> / Azure CLI у GitHub Actions.</li>
<li>Усунення попереджень Windows SmartScreen («Windows protected your PC») та запобігання хибним спрацюванням Windows Defender.</li>
</ul>
</li>
<li><strong>Офлайн-генератор та валідатор ліцензійних ключів:</strong>
<ul>
<li>Криптографічна схема на базі асиметричного підпису Ed25519 (приватний ключ на стороні платіжного шлюзу, публічний ключ зашитий у бінарник).</li>
<li>Повна автономність: перевірка ключа працює без звернення до серверів у реальному часі.</li>
</ul>
</li>
<li><strong>Адаптери релізних каналів (Release Channel Adapters):</strong>
<ul>
<li>Модуль <code>gametrimmer-steam</code>: зв'язок зі Steamworks API.</li>
<li>Модуль <code>gametrimmer-standalone</code>: вбудований перевірочник нових версій через GitHub Releases API / локальний чек.</li>
<li>Модуль <code>gametrimmer-msix</code>: нативна інтеграція з Microsoft Store.</li>
</ul>
</li>
</ol>
<hr/>
<ul data-type="taskList">
<li data-type="taskItem" data-checked="false"><p>Протестувати Ed25519 валідацію ліцензійних ключів у Rust.</p></li>
<li data-type="taskItem" data-checked="false"><p>Дослідити вимоги та вартість Microsoft Azure Trusted Signing для Rust-бінарників.</p></li>
<li data-type="taskItem" data-checked="false"><p>Підготувати архітектурну схему збірок для різних дистрибутивних платформ.</p></li>
</ul>"""
    }
]

api_url = "http://127.0.0.1:3456/api/v2/projects/5/tasks"

for t in tasks:
    data = json.dumps(t).encode("utf-8")
    req = urllib.request.Request(api_url, data=data, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req) as resp:
            resp_data = json.loads(resp.read().decode("utf-8"))
            print(f"SUCCESS: Created task {resp_data.get('id')} - {resp_data.get('title')}")
    except urllib.error.HTTPError as e:
        err_msg = e.read().decode("utf-8")
        print(f"ERROR creating task '{t['title']}': HTTP {e.code} - {err_msg}")
