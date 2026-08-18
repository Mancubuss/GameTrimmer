import re

def update_file(path):
    with open(path, "r", encoding="utf-8") as f:
        content = f.read()

    # Replace (Lang::En, with (Lang::En | Lang::Custom(_),
    content = re.sub(r'\(Lang::En,', '(Lang::En | Lang::Custom(_),', content)
    # Replace (Lang::En | Lang::Uk, with (Lang::En | Lang::Uk | Lang::Custom(_),
    content = re.sub(r'\(Lang::En \| Lang::Uk,', '(Lang::En | Lang::Uk | Lang::Custom(_),', content)
    
    # Replace `Lang::En =>` with `Lang::En | Lang::Custom(_) =>`
    # Be careful not to double replace if already replaced
    content = re.sub(r'(?<!\|\s)Lang::En\s*=>', 'Lang::En | Lang::Custom(_) =>', content)

    with open(path, "w", encoding="utf-8") as f:
        f.write(content)

update_file(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\crates\app\src\i18n\messages.rs")
update_file(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\crates\app\src\worker\rules_io.rs")
print("Updated messages.rs and rules_io.rs with fallback for Lang::Custom(_)")
