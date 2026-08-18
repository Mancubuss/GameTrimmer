import subprocess
import urllib.request
import json
import sys

sys.stdout.reconfigure(encoding='utf-8')

ps_cmd = """
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\\Mancubus\\Projects\\Vibecoding\\Vikunja\\antigravity-agent.api-token.dpapi")
$unprotected = [System.Security.Cryptography.ProtectedData]::Unprotect($bytes, $null, [System.Security.Cryptography.DataProtectionScope]::CurrentUser)
[System.Text.Encoding]::UTF8.GetString($unprotected)
"""

res = subprocess.run(["powershell", "-NoProfile", "-Command", ps_cmd], capture_output=True, text=True, check=True)
token = res.stdout.strip()

headers = {
    "Authorization": f"Bearer {token}",
    "Content-Type": "application/json; charset=utf-8",
    "Accept": "application/json"
}

# Delete task 334 if exists to keep clean state
try:
    req_del = urllib.request.Request("http://127.0.0.1:3456/api/v2/tasks/334", headers=headers, method="DELETE")
    urllib.request.urlopen(req_del)
    print("Deleted prior task 334.")
except Exception as e:
    print("Delete error:", e)

task_data = {
    "title": "GT-157 · [Маркетинг] Покроковий маркетинговий план запуску та просування GameTrimmer у Steam (Go-To-Market Strategy)",
    "priority": 3,
    "labels": [{"id": 39}, {"id": 37}],
    "description": """<p><strong>МАРКЕТИНГОВИЙ ПЛАН: Комплексна Go-To-Market стратегія виходу GameTrimmer у Steam (Pay-Once Premium $6.99 + Open-Core на GitHub)</strong></p>
<p><strong>Мета картки:</strong> Забезпечити виконання базового/оптимістичного фінансового сценарію (25,000–100,000 продажів, $66,750–$267,000 чистого прибутку ФОП) за рахунок органічного залучення, накопичення 7,000–10,000 вішлістів до релізу, інфлюенсер-аутрічу серед власників Steam Deck та запуску віральної механіки.</p>
<hr/>
<h3>1. Позиціонування, Меседжі та УТП</h3>
<ul>
<li><strong>Ключовий хук:</strong> <em>«Reclaim 50–100+ GB of NVMe/SSD space in minutes without uninstalling games or triggering anti-cheat bans.»</em></li>
<li><strong>Ключові стовпи (Pillars):</strong>
<ul>
<li><strong>Неймовірна швидкість:</strong> 100% Native Rust, без Electron, без рантайм-сміття, миттєве сканування терабайтних бібліотек.</li>
<li><strong>100% Anti-Cheat & VAC Safe:</strong> Нуль втручань у процес гри, нульова модифікація пам'яті/exe, тільки безпечні пасивні залишки та кеші.</li>
<li><strong>Кілер-фіча для Steam Deck:</strong> Повертає десятки гігабайтів на девайсах з 512GB/64GB+SD без потреби купувати дорогі накопичувачі.</li>
<li><strong>Чесна модель:</strong> Одноразова покупка $6.99 (з регіональними цінами) без жодних підписок, реклами чи збору приватних даних.</li>
</ul>
</li>
</ul>
<hr/>
<h3>2. Сегментація цільової аудиторії</h3>
<ol>
<li><strong>Сегмент A: Власники Steam Deck, ROG Ally, Legion Go (Tier-1 пріоритет):</strong> Найвища конверсія через гострий дефіцит внутрішньої пам'яті (AAA ігри 100–150GB займають до 30% всього диска).</li>
<li><strong>Сегмент B: Хардкорні PC-геймери з великими бібліотеками (Tier-1):</strong> Гравці з 50+ іграми на SSD, яким складно перекачувати 100GB через ліміти або швидкість мережі.</li>
<li><strong>Сегмент C: Ентузіасти заліза та оптимізатори (Tier-2):</strong> Аудиторія r/pcmasterrace, r/lowendgaming, що прагне витиснути максимум із кожного гігабайта.</li>
<li><strong>Сегмент D: FOSS & Privacy свідома спільнота (Tier-2):</strong> Лояльні користувачі, які цінують відкрите ядро на GitHub та підтримку незалежного автора.</li>
</ol>
<hr/>
<h3>3. Покрокові етапи маркетингової кампанії (Roadmap запуску)</h3>
<h4>Етап 1: Підготовка та передрелізний рушій вішлістів (Pre-Launch & Wishlist Engine, T-60 ... T-0 днів)</h4>
<ul>
<li><strong>Оптимізація сторінки в Steam:</strong> Дизайн яскравих капсул (Header, Small, Main Capsule), геймплейні GIF «До/Після» (Baldur's Gate 3: -24GB, Cyberpunk: -18GB), Anti-Cheat Safe бейдж та зрозумілий текст без технічного перевантаження.</li>
<li><strong>Open-Core Community Preview (GitHub/itch.io):</strong> Безкоштовний CLI/Core білд із банером у консолі/логах: <em>«Want one-click auto-trimming & Steam Deck UI? Wishlist GameTrimmer on Steam!»</em></li>
<li><strong>Органічний посів у профільних спільнотах:</strong> Публікації практичних кейсів оптимізації в r/SteamDeck, r/pcgaming, r/rust, на SteamDeckHQ, Deckverse, GamingOnLinux, Overkill.wtf.</li>
<li><strong>Wishlist Milestones:</strong> Оголошення спільноті цілей (наприклад: 5k вішлістів → -15% знижка на релізі; 10k → підтримка додаткових 20 монолітних ігор). Ціль: <strong>7,000–10,000 вішлістів</strong> до старту.</li>
</ul>
<h4>Етап 2: Релізний вибух та конверсія (Launch Blast Week, T-0 ... T+14 днів)</h4>
<ul>
<li><strong>Launch Discount:</strong> Запуск із вітальною знижкою -15% ($5.94) для активації розсилки сповіщень усім користувачам із вішлістами (очікувана конверсія 15–25%).</li>
<li><strong>Інфлюенсер-аутріч:</strong> Розсилка 50–100 безкоштовних ключів через Steam Curator Connect та YouTube авторам (тематики: Steam Deck optimization, PC hardware, budget gaming) із готовими хуками.</li>
<li><strong>Прес-кампанія:</strong> Розсилка прес-релізів до PC Gamer, Wccftech, Tom's Hardware, Rock Paper Shotgun, TechPowerUp, Mezha.Media, ITC.ua, DOU.</li>
<li><strong>Внутрішньопрограмна віральність (Viral Loop):</strong> Кнопка <em>«Share Space Saved»</em> у додатку, яка генерує привабливу картку результату (<em>«GameTrimmer just saved me 72.4 GB!»</em>) для Discord, Twitter, Reddit.</li>
</ul>
<h4>Етап 3: Алгоритмічний ріст та утримання рейтингу (Post-Launch, T+15 ... T+90 днів)</h4>
<ul>
<li><strong>Захист рейтингу «Overwhelmingly Positive» (95%+):</strong> Особисті відповіді розробника на 100% негативних коментарів, виправлення багів протягом 24–48 годин.</li>
<li><strong>Участь у сезонних розпродажах Steam:</strong> Синхронізація зі Steam Summer/Autumn/Winter Sale (-20%..-25%).</li>
<li><strong>Хаб користувацьких пресетів:</strong> Запуск рецептів оптимізації через Steam Community Hub / Workshop.</li>
</ul>
<h4>Етап 4: Масштабування та додаткові канали (Місяці 3–12+)</h4>
<ul>
<li><strong>Microsoft Store (MSIX):</strong> Реліз для ПК-геймерів за межами Steam (користувачі PC Game Pass).</li>
<li><strong>Плагін для Decky Loader:</strong> Нативний плагін швидкого очищення безпосередньо в інтерфейсі Steam Deck Gaming Mode.</li>
<li><strong>B2B пакети для інтернет-кафе та клубів:</strong> Корпоративні ліцензії на масове очищення локальних сховищ ігрових комп'ютерів.</li>
</ul>
<hr/>
<h3>4. Чек-лист маркетингових завдань</h3>
<ul data-type=\"taskList\">
<li data-type=\"taskItem\" data-checked=\"false\"><p>Підготувати дизайн-пак для Steam Store (Capsule banners, screenshots з бенчмарками, логотип, трейлер).</p></li>
<li data-type=\"taskItem\" data-checked=\"false\"><p>Скласти базу контактів 100+ інфлюенсерів (Steam Deck, PC gaming, hardware tech YouTubers).</p></li>
<li data-type=\"taskItem\" data-checked=\"false\"><p>Оформити прес-кіт (Press Kit) з high-res асетами, описом технології та прямими контактами розробника.</p></li>
<li data-type=\"taskItem\" data-checked=\"false\"><p>Опублікувати Coming Soon сторінку в Steam та запустити першу хвилю збору вішлістів.</p></li>
<li data-type=\"taskItem\" data-checked=\"false\"><p>Організувати публікації в r/SteamDeck та профільних медіа до релізу.</p></li>
<li data-type=\"taskItem\" data-checked=\"false\"><p>Запустити реліз із 15% Launch Discount та розіслати оглядові ключі пресі.</p></li>
<li data-type=\"taskItem\" data-checked=\"false\"><p>Впровадити в інтерфейс кнопку вірального шерингу результатів оптимізації.</p></li>
<li data-type=\"taskItem\" data-checked=\"false\"><p>Підготувати розсилку до першого великого сезонного розпродажу Steam.</p></li>
</ul>"""
}

api_url = "http://127.0.0.1:3456/api/v2/projects/5/tasks"
data = json.dumps(task_data).encode("utf-8")
req = urllib.request.Request(api_url, data=data, headers=headers, method="POST")

with urllib.request.urlopen(req) as resp:
    resp_data = json.loads(resp.read().decode("utf-8"))
    print(f"SUCCESS: Created task ID {resp_data.get('id')} - {resp_data.get('title')}")
