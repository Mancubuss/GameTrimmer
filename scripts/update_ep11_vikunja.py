import subprocess
import urllib.request
import json
import sys

sys.stdout.reconfigure(encoding='utf-8')

ps_cmd = r"""
Add-Type -AssemblyName System.Security
$bytes = [System.IO.File]::ReadAllBytes("E:\Mancubus\Projects\Vibecoding\Vikunja\antigravity-agent.api-token.dpapi")
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

# Clean up duplicate 320 if present
try:
    req = urllib.request.Request("http://127.0.0.1:3456/api/v2/tasks/320", headers=headers, method="DELETE")
    urllib.request.urlopen(req)
    print("Deleted duplicate task 320.")
except Exception as e:
    print(f"Note on 320: {e}")

comments = {
    321: """<p><strong>ПІДСУМКИ ДОСЛІДЖЕННЯ ЕПІКУ 11:</strong></p>
<p>Завершено повний комплекс досліджень за 5 спайками. Сформовано детальний звіт та інтерактивний план релізу у файлі <code>docs/commercialization-plan.html</code>.</p>
<ul>
<li><strong>Ліцензійна чистота:</strong> 100% Permissive дерево в <code>Cargo.lock</code> (MIT, Apache-2.0, BSD-3, CC0, SQLite). Відсутність Copyleft ризиків.</li>
<li><strong>Рекомендована модель:</strong> Pay-Once Premium у Steam ($6.99) за схемою Open-Core (вільне ядро <code>gametrimmer-core</code> на GitHub під MIT + закритий комерційний білд у Steam).</li>
<li><strong>Прогноз доходів (Рік 1):</strong> ~$66,750 (~2.77M грн) чистого прибутку на рахунок ФОП у базовому сценарії (25,000 продажів). Накопичувально за 3 роки: ~$200,250 (~8.31M грн).</li>
<li><strong>Податки:</strong> W-8BEN знижує US Withholding Tax до 10%. ФОП 3-тя група (5% ЄП + ВЗ). Підсумковий Take-Home чистий дохід — $2.67 / 110.8 грн з кожної копії.</li>
</ul>""",

    322: """<p><strong>ВИСНОВКИ СПАЙКУ GT-147 (ЛІЦЕНЗУВАННЯ):</strong></p>
<ul>
<li><strong>Аудит crates:</strong> <code>rusqlite</code>, <code>windows-rs</code>, <code>egui/eframe</code>, <code>rayon</code>, <code>ntfs</code>, <code>trash</code>, <code>zip</code> мають виключно пермісивні ліцензії. Немає жодного компонента під GPL/AGPL/LGPL.</li>
<li><strong>Право переліцензування:</strong> 100% коду написано автором; автор має повне право випускати версії 1.1+ як комерційний пропрієтарний софт.</li>
<li><strong>CLA/DCO:</strong> Підготовлено тексти для <code>CONTRIBUTING.md</code> (включені в звіт та HTML).</li>
</ul>""",

    323: """<p><strong>ВИСНОВКИ СПАЙКУ GT-148 (STEAMWORKS):</strong></p>
<ul>
<li><strong>Steam Direct:</strong> $100 fee повертається при $1,000 виторгу. Заповнення W-8BEN (ст. 12 конвенції США-Україна) знижує податок до 10%.</li>
<li><strong>Легітимність за SSA/SDA:</strong> Категорія Software → Utilities. Очищення та мікро-заглушки безпечні, діють суто за згодою користувача, файли відновлюються перевіркою цілісності в Steam.</li>
<li><strong>Steamworks SDK:</strong> Реалізовано архітектуру Graceful Init через <code>steamworks-rs</code>. Бінарник запускається як у клієнті Steam, так і без нього.</li>
<li><strong>Steam Pipe CI/CD:</strong> Складено готовий GitHub Actions workflow для деплою релізів.</li>
</ul>""",

    324: """<p><strong>ВИСНОВКИ СПАЙКУ GT-149 (АЛЬТЕРНАТИВНІ КАНАЛИ):</strong></p>
<ul>
<li><strong>Microsoft Store (MSIX):</strong> Миттєва 100% довіра Windows SmartScreen без витрат на дорогі EV Code Signing сертифікати. Маніфест з <code>broadFileSystemAccess</code> підготовлено.</li>
<li><strong>itch.io:</strong> Готовий скрипт для <code>butler push</code> у форматі Pay-What-You-Want.</li>
<li><strong>D2C через MoR (Lemon Squeezy / Paddle):</strong> Платформа бере на себе податкову звітність і сплату VAT у 100+ країнах. Один чистий B2B переказ на місяць.</li>
</ul>""",

    325: """<p><strong>ВИСНОВКИ СПАЙКУ GT-150 (ЮРИДИЧНИЙ КОМПЛАЄНС & EULA):</strong></p>
<ul>
<li><strong>Commercial EULA:</strong> Повний текст AS-IS та обмеження відповідальності складено.</li>
<li><strong>Anti-Cheat Safe Shield:</strong> Офіційний дисклеймер: утиліта не інжектує пам'ять, не змінює .exe і працює тільки при закритій грі (безпечно для VAC, EAC, BattlEye, Vanguard).</li>
<li><strong>Nominative Fair Use:</strong> Дисклеймер щодо сторонніх торговельних марок Steam, Epic, Ubisoft, EA готовий.</li>
<li><strong>Податкова форма:</strong> ФОП 3-тя група (5% ЄП), КВЕД 58.29, 62.01, 62.02. ЗЕД-контракти за Законом № 4496.</li>
</ul>""",

    326: """<p><strong>ВИСНОВКИ СПАЙКУ GT-151 (РЕЛІЗ-ІНЖИНІРИНГ):</strong></p>
<ul>
<li><strong>Code Signing:</strong> Рекомендовано Azure Trusted Signing (~$10/міс) для standalone .exe або MSIX у MS Store (безкоштовно) замість USB EV-токенів ($400+/рік).</li>
<li><strong>Офлайн-ліцензування:</strong> Реалізовано та протестовано повний Rust-модуль валідації ліцензійних сертифікатів на базі Ed25519 (100% автономна офлайн-робота).</li>
<li><strong>Auto-Updater:</strong> Механізм заміни виконуваного файлу через атомне перейменування у Win32 без блокування процесу.</li>
</ul>"""
}

for task_id, comment_text in comments.items():
    data = json.dumps({"comment": comment_text}).encode("utf-8")
    req = urllib.request.Request(f"http://127.0.0.1:3456/api/v2/tasks/{task_id}/comments", data=data, headers=headers, method="PUT")
    try:
        with urllib.request.urlopen(req) as resp:
            print(f"SUCCESS: Commented on task {task_id}")
    except urllib.error.HTTPError as e:
        # try POST if PUT not supported
        req_post = urllib.request.Request(f"http://127.0.0.1:3456/api/v2/tasks/{task_id}/comments", data=data, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req_post) as resp_post:
                print(f"SUCCESS (POST): Commented on task {task_id}")
        except Exception as ex:
            print(f"ERROR on task {task_id}: {ex}")

print("Vikunja update completed.")
