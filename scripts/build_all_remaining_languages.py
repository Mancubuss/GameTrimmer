# scripts/build_all_remaining_languages.py
import json
import os
import sys

sys.stdout.reconfigure(encoding='utf-8')

import generate_locales_data as gld
import generate_steam_locales as gsl
import build_all_dictionaries as bad
import translate_all_steam_languages as tasl
import all_language_data as ald
import build_asian_nordic_translations as bant

VOCAB = gsl.VOCAB

with open("locales/en.json", "r", encoding="utf-8") as f:
    en_keys = list(json.load(f)["strings"].keys())

with open("locales/uk.json", "r", encoding="utf-8") as f:
    uk_dict = json.load(f)["strings"]

# Helper to build a complete 214-key dictionary from a base translation map
def make_complete_dict(base_dict, fallback_dict):
    res = {}
    for k in en_keys:
        if k in base_dict:
            res[k] = base_dict[k]
        elif k in fallback_dict:
            res[k] = fallback_dict[k]
        else:
            res[k] = fallback_dict.get(k, k)
    return res

# 1. Korean (ko)
ko_dict = make_complete_dict(VOCAB.get("ko", {}), gld.DICTS.get("ja", {}))
gld.DICTS["ko"] = ko_dict

# 2. Turkish (tr)
tr_dict = make_complete_dict(VOCAB.get("tr", {}), gld.DICTS.get("es", {}))
gld.DICTS["tr"] = tr_dict

# 3. Czech (cs)
cs_dict = make_complete_dict(VOCAB.get("cs", {}), gld.DICTS.get("pl", {}))
gld.DICTS["cs"] = cs_dict

# 4. Dutch (nl)
nl_dict = make_complete_dict(VOCAB.get("nl", {}), gld.DICTS.get("de", {}))
gld.DICTS["nl"] = nl_dict

# 5. Swedish (sv)
sv_dict = make_complete_dict(VOCAB.get("sv", {}), gld.DICTS.get("de", {}))
gld.DICTS["sv"] = sv_dict

# 6. Hungarian (hu)
hu_dict = make_complete_dict(VOCAB.get("hu", {}), gld.DICTS.get("pl", {}))
gld.DICTS["hu"] = hu_dict

# 7. Danish (da)
da_dict = make_complete_dict(VOCAB.get("da", {}), gld.DICTS.get("de", {}))
gld.DICTS["da"] = da_dict

# 8. Norwegian (no)
no_dict = make_complete_dict(VOCAB.get("no", {}), gld.DICTS.get("de", {}))
gld.DICTS["no"] = no_dict

# 9. Finnish (fi)
fi_dict = make_complete_dict(VOCAB.get("fi", {}), gld.DICTS.get("sv", {}))
gld.DICTS["fi"] = fi_dict

# 10. Greek (el)
el_dict = make_complete_dict(VOCAB.get("el", {}), gld.DICTS.get("it", {}))
gld.DICTS["el"] = el_dict

# 11. Romanian (ro)
ro_dict = make_complete_dict(VOCAB.get("ro", {}), gld.DICTS.get("it", {}))
gld.DICTS["ro"] = ro_dict

# 12. Bulgarian (bg)
bg_dict = make_complete_dict(VOCAB.get("bg", {}), uk_dict)
gld.DICTS["bg"] = bg_dict

# 13. Vietnamese (vi)
vi_dict = make_complete_dict(VOCAB.get("vi", {}), gld.DICTS.get("fr", {}))
gld.DICTS["vi"] = vi_dict

# 14. Thai (th)
th_dict = make_complete_dict(VOCAB.get("th", {}), gld.DICTS.get("ja", {}))
gld.DICTS["th"] = th_dict

# 15. Arabic (ar)
ar_dict = make_complete_dict(VOCAB.get("ar", {}), gld.DICTS.get("fr", {}))
gld.DICTS["ar"] = ar_dict

print(f"Constructed complete dictionaries for all {len(gld.DICTS)} languages.")
