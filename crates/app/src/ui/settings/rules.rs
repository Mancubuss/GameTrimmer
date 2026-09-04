//! "Rules": whether either optional overlay pack is lying next to the
//! executable, and whether the one that is there still parses.
//!
//! There is nothing to press here. The rules a scan runs on are compiled
//! into the app, so they are always current and there is nothing to restore
//! them from or export them to. An overlay is in effect because someone put
//! a file of that name next to the executable - and that is exactly why this
//! readout has to exist: an overlay that does not parse is ignored, and
//! being ignored looks precisely like being wrong about the library.

use eframe::egui;

use gametrimmer_core::packs::PackKind;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::ui::row_actions;
use crate::worker::{self, rules_io};

use super::SUCCESS_GREEN;

/// The packs, in the order the section lists them, with the file each one
/// lives in - the file name is the whole instruction for putting one there.
fn packs(s: &i18n::Strings) -> [(PackKind, &'static str, &'static str); 2] {
    [
        (
            PackKind::CategoryRules,
            s.rules_pack_category_label,
            worker::RULES_FILE_NAME,
        ),
        (
            PackKind::LangPack,
            s.rules_pack_lang_label,
            worker::L10N_RULES_FILE_NAME,
        ),
    ]
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let _ = app;
    let s = i18n::strings(app.lang());

    for (kind, label, file_name) in packs(s) {
        show_pack(ui, s, kind, label, file_name);
        ui.add_space(10.0);
    }

    ui.small(s.rules_hint);
}

/// What the line beside a pack's name says, and whether it is an error.
///
/// Split out from the drawing so the three-way mapping can be tested without
/// depending on which files happen to sit next to the test binary - the
/// distinction that matters is "no file" versus "broken file", and reading a
/// missing overlay as a broken one is the mistake worth guarding against.
fn state_line<'a>(s: &'a i18n::Strings, present: bool, valid: bool) -> (&'a str, bool) {
    match (present, valid) {
        (false, _) => (s.rules_pack_absent_label, false),
        (true, true) => (s.rules_valid_label, false),
        (true, false) => (s.rules_invalid_label, true),
    }
}

/// One pack: its name, whether a file is there at all, whether that file
/// parses, and where it would go.
fn show_pack(ui: &mut egui::Ui, s: &i18n::Strings, kind: PackKind, label: &str, file_name: &str) {
    let state = pack_state(ui.ctx(), kind, file_name);
    ui.horizontal(|ui| {
        ui.strong(label);
        let (line, is_error) = state_line(s, state.present, state.valid);
        if is_error {
            ui.colored_label(ui.visuals().error_fg_color, line);
        } else if state.present {
            ui.colored_label(SUCCESS_GREEN, line);
        } else {
            ui.label(line);
        }
    });
    // Where to look - to fix a pack that does not parse, or to put one there
    // in the first place. Same reason the database path is shown in
    // "Data & diagnostics".
    if let Some(path) = &state.path {
        ui.small(row_actions::windows_path_string(path));
    }
}

/// A pack's readout: where its file would be, whether it is there, and
/// whether it parses.
#[derive(Clone)]
struct PackState {
    path: Option<std::path::PathBuf>,
    present: bool,
    valid: bool,
    checked: std::time::Instant,
}

/// How long a readout is reused before the files are consulted again.
///
/// Answering "does this pack still parse?" is not cheap: it resolves the exe
/// directory, stats the file, reads it, and parses the result - which for the
/// category pack means building a whole `RuleEngine`, regexes and all. That
/// ran twice per pack on *every frame* while this section was open, so the
/// dialog paid for four file reads and two engine builds per repaint, and a
/// frame took as long as the slowest of those - seconds, when the reads land
/// on a cold disk or an on-access virus scanner.
///
/// A second is chosen so the readout is still what the module docs promise -
/// live, tracking a file dropped in or hand-edited while the dialog is open -
/// at a fraction of the cost.
pub(crate) const PACK_STATE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

fn pack_state_id(file_name: &str) -> egui::Id {
    egui::Id::new(("gametrimmer.rules_pack_state", file_name))
}

fn pack_state(ctx: &egui::Context, kind: PackKind, file_name: &str) -> PackState {
    let id = pack_state_id(file_name);
    if let Some(cached) = ctx.data(|data| data.get_temp::<PackState>(id)) {
        if cached.checked.elapsed() < PACK_STATE_TTL {
            return cached;
        }
    }

    let present = rules_io::pack_is_present(kind);
    let state = PackState {
        path: rules_io::pack_path(kind).ok(),
        present,
        valid: present && rules_io::pack_is_valid(kind),
        checked: std::time::Instant::now(),
    };
    ctx.data_mut(|data| {
        data.insert_temp(id, state.clone());
        // Test-only bookkeeping, kept in the context rather than in a global
        // so tests running in parallel cannot see each other's counts.
        #[cfg(test)]
        {
            let reads = data.get_temp::<usize>(pack_read_count_id()).unwrap_or(0);
            data.insert_temp(pack_read_count_id(), reads + 1);
        }
    });
    state
}

#[cfg(test)]
fn pack_read_count_id() -> egui::Id {
    egui::Id::new("gametrimmer.rules_pack_reads")
}

/// How many times this context has actually gone to disk for a pack readout -
/// the measurement behind the "this section does not re-read its packs every
/// frame" test, which is the deterministic half of the frame-cost budgets in
/// `ui::perf`.
#[cfg(test)]
pub(crate) fn pack_disk_reads(ctx: &egui::Context) -> usize {
    ctx.data(|data| data.get_temp::<usize>(pack_read_count_id()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use crate::ui::harness::UiTest;
    use crate::ui::settings::SettingsSection;

    fn open_rules() -> UiTest {
        let mut test = UiTest::new(crate::ui::settings::show);
        test.app_mut().show_settings = true;
        test.app_mut().settings_section = SettingsSection::Rules;
        test.run();
        test
    }

    #[test]
    fn the_section_lists_both_packs() {
        let test = open_rules();
        let s = test.strings();

        test.assert_label(s.rules_pack_category_label);
        test.assert_label(s.rules_pack_lang_label);
    }

    /// The section's reason to exist: each pack says what state it is in.
    /// Nothing writes an overlay next to the test binary, so on a clean run
    /// both read as absent - and "absent" has to be a state the section can
    /// say, or a fresh install would read as two broken packs.
    #[test]
    fn every_pack_reports_its_state() {
        let test = open_rules();
        let s = test.strings();

        assert_eq!(
            test.count_labels(s.rules_pack_absent_label)
                + test.count_labels(s.rules_valid_label)
                + test.count_labels(s.rules_invalid_label),
            2,
            "each of the two packs must carry exactly one state readout",
        );
    }

    /// The counter-example to the test above: it would pass just as happily
    /// if every pack read "does not parse". A missing overlay is the normal
    /// state and must never be dressed up as a broken one - that would send
    /// someone hunting a syntax error in a file they never wrote. Asserted on
    /// the mapping rather than on a rendered frame, because what sits next to
    /// the test binary is whatever earlier builds left there.
    #[test]
    fn a_pack_with_no_file_reads_as_absent_not_as_broken() {
        let s = crate::i18n::strings(crate::i18n::Lang::En);

        assert_eq!(
            super::state_line(s, false, false),
            (s.rules_pack_absent_label, false),
            "a missing overlay was reported as a broken one",
        );
        assert_eq!(
            super::state_line(s, true, false),
            (s.rules_invalid_label, true),
            "an overlay that does not parse must be reported as an error",
        );
        assert_eq!(
            super::state_line(s, true, true),
            (s.rules_valid_label, false),
        );
    }
}
