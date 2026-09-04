//! The two rule-pack kinds, and the one text-level write the app makes to a
//! pack: adding a personal exception.
//!
//! The app layer only shuttles file contents around; everything that needs
//! JSON lives here so it is unit-testable and the app crate needs no serde
//! dependency.

use crate::error::Result;
use crate::rules::{parse_rule_list, serialize_rule_list, Rule, RuleEngine};

/// Which of the two pack files a file is - the two optional overlays a user
/// may put next to the executable (see `docs/rules-packs.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackKind {
    /// `rules.json` - `{"version", "rules"}`.
    CategoryRules,
    /// `l10n_rules.json` - `{"version", "languages", ...}`.
    LangPack,
}

/// Whether two rules are the same rule.
///
/// Identity is `(polarity, category, scope, pattern)`. A keep rule and a
/// deleting rule over the same pattern are opposites, not versions of each
/// other, and the same pattern scoped to two different games is two different
/// rules - collapsing either pair would silently drop one of them.
fn same_rule(a: &Rule, b: &Rule) -> bool {
    a.polarity == b.polarity
        && a.category == b.category
        && a.app_id == b.app_id
        && a.pattern == b.pattern
}

/// Adds one rule to a pack, unless the pack already holds it.
///
/// This is how the personal exception pack grows: the app reads the file,
/// calls this, and writes the result back. `false` means the rule was already
/// there - the caller says "already kept" rather than writing an identical
/// second copy, which is what right-clicking the same file twice would
/// otherwise produce.
///
/// The result is validated by compiling it: a personal pack that no longer
/// parses would take the *scan's* whole rule set down with it (see
/// `worker::scan::run_scan`), so it must never be written broken in the first
/// place.
pub fn add_rule(base_json: &str, rule: Rule) -> Result<(String, bool)> {
    let mut rules = parse_rule_list(base_json)?;
    if rules.iter().any(|existing| same_rule(existing, &rule)) {
        return Ok((serialize_rule_list(&rules)?, false));
    }
    rules.push(rule);
    let json = serialize_rule_list(&rules)?;
    RuleEngine::from_json(&json)?;
    Ok((json, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a bare rule array in the pack envelope, so a test about pack
    /// *semantics* does not restate the wrapper each time.
    fn pack(rules: &str) -> String {
        format!(r#"{{"version":1,"rules":{rules}}}"#)
    }

    /// Spelled out rather than imported under a short name so the test reads
    /// as what it is: the constructor the UI's "never touch this" writes with.
    fn keep_rule(app_id: &str, rel_path: &str, desc: &str) -> Rule {
        Rule::keep_file(app_id, rel_path, desc.into())
    }

    /// Right-clicking the same file twice must not grow the pack twice, and
    /// the caller has to be able to tell the user which of the two happened.
    #[test]
    fn adding_the_same_exception_twice_changes_nothing_the_second_time() {
        let rule = keep_rule("620", r"Support\ru\voices.pak", "Kept by me");

        let (once, added) = add_rule(&pack("[]"), rule.clone()).expect("the first add succeeds");
        assert!(added);
        assert_eq!(parse_rule_list(&once).unwrap().len(), 1);

        let (twice, added) = add_rule(&once, rule).expect("the second add succeeds");
        assert!(!added, "an identical exception must not be appended again");
        assert_eq!(parse_rule_list(&twice).unwrap().len(), 1);
    }

    /// The scope is part of a rule's identity: the same path kept in two games
    /// is two exceptions, and merging them into one would silently unprotect
    /// one of the games.
    #[test]
    fn the_same_path_kept_in_two_games_stays_two_rules() {
        let path = r"Support\ru\voices.pak";
        let (first, _) = add_rule(&pack("[]"), keep_rule("620", path, "Kept in one")).unwrap();
        let (both, added) = add_rule(&first, keep_rule("730", path, "Kept in the other")).unwrap();

        assert!(added);
        assert_eq!(parse_rule_list(&both).unwrap().len(), 2);
    }
}
