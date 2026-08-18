import re

with open(r"e:\Mancubus\Projects\Vibecoding\GameTrimmer\crates\app\src\i18n\mod.rs", "r", encoding="utf-8") as f:
    content = f.read()

# Extract field names from struct Strings
match = re.search(r'pub struct Strings \{([\s\S]*?)\n\}', content)
if not match:
    raise ValueError("Could not find struct Strings in mod.rs")

struct_body = match.group(1)
fields = re.findall(r'pub\s+([a-z0-9_]+)\s*:\s*&\'static str', struct_body)
print(f"Found {len(fields)} fields in Strings struct.")

# Build apply_overrides code
overrides_lines = []
for field in fields:
    overrides_lines.append(f'        if let Some(val) = map.get("{field}") {{ s.{field} = Box::leak(val.clone().into_boxed_str()); }}')

apply_overrides_fn = f"""
impl Strings {{
    pub fn apply_overrides(&self, map: &std::collections::HashMap<String, String>) -> Strings {{
        let mut s = *self;
{chr(10).join(overrides_lines)}
        s
    }}
}}
"""

# Let's inspect where to insert apply_overrides_fn and update strings(lang: Lang)
print("apply_overrides built successfully.")
