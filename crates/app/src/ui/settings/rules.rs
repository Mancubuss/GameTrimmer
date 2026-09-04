//! "Rules": that GameTrimmer can be extended with a rule file of your own,
//! where to put one, and - only when one is actually there - whether it is
//! in effect.
//!
//! There is nothing to press and nothing to configure. The rules a scan runs
//! on are compiled into the app, so there is nothing here to restore them
//! from or export them to, and no reason for this section to describe them
//! at all. An overlay next to the executable is the one thing about rules a
//! user can act on, so it is the only thing this section talks about.
//!
//! The state readout exists for one reason: an overlay that does not parse
//! is ignored, and being ignored looks exactly like being wrong about the
//! library. Without a line saying so, a broken pack is a silent one.

use eframe::egui;

use gametrimmer_core::packs::PackKind;

use crate::app::GameTrimmerApp;
use crate::i18n;
use crate::ui::row_actions;
use crate::worker::{self, rules_io};

use super::SUCCESS_GREEN;

/// The two file names an overlay can have, in the order the section lists
/// them. The file name is the whole instruction: putting a file with that
/// name in the folder is the entire mechanism.
fn pack_files() -> [(PackKind, &'static str); 2] {
    [
        (PackKind::CategoryRules, worker::RULES_FILE_NAME),
        (PackKind::LangPack, worker::L10N_RULES_FILE_NAME),
    ]
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let s = i18n::strings(app.lang());

    ui.label(s.rules_hint);
    ui.add_space(8.0);
    show_folder(ui, s);

    // Only packs that are actually there get a line. On the overwhelmingly
    // common install nothing follows, which is the honest answer: there is
    // no overlay, and the built-in rules need no reporting on.
    let found: Vec<(PackKind, &str)> = pack_files()
        .into_iter()
        .filter(|(kind, file_name)| pack_state(ui.ctx(), *kind, file_name).present)
        .collect();
    if found.is_empty() {
        return;
    }

    ui.add_space(10.0);
    ui.label(s.rules_found_label);
    for (kind, file_name) in found {
        ui.horizontal(|ui| {
            ui.strong(file_name);
            if pack_state(ui.ctx(), kind, file_name).valid {
                ui.colored_label(SUCCESS_GREEN, s.rules_valid_label);
            } else {
                ui.colored_label(ui.visuals().error_fg_color, s.rules_invalid_label);
            }
        });
    }
}

/// The folder an overlay goes in, with the same copy/open pair the database
/// path gets in "Data & diagnostics" - "where do I put one?" is the section's
/// first question, and a path you cannot open is only half an answer.
fn show_folder(ui: &mut egui::Ui, s: &i18n::Strings) {
    let Ok(path) = rules_io::pack_path(PackKind::CategoryRules) else {
        return;
    };
    let Some(folder) = path.parent() else {
        return;
    };
    let display_path = row_actions::windows_path_string(folder);

    // Truncated rather than wrapped, for the same reason as the database
    // path: a deep path would push the buttons out of the viewport, and the
    // full text is one click away on "Copy".
    ui.add(egui::Label::new(&display_path).truncate());
    ui.horizontal(|ui| {
        if ui.button(s.btn_copy).clicked() {
            ui.ctx().copy_text(display_path.clone());
        }
        if ui.button(s.btn_open_folder).clicked() {
            let (program, args) = row_actions::open_folder_args(folder);
            if let Err(err) = row_actions::launch(program, &args) {
                crate::logger::error(&format!("Failed to open Explorer: {err}"));
            }
        }
    });
}

/// A pack's readout: whether a file is there, and whether it parses.
#[derive(Clone)]
struct PackState {
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

    /// What the section is for: say that an overlay is possible and where it
    /// goes. Both of those must be on screen with no file present.
    #[test]
    fn the_section_explains_the_overlay_and_where_to_put_one() {
        let test = open_rules();
        let s = test.strings();

        test.assert_label(s.rules_hint);
        test.assert_label(s.btn_open_folder);
    }

    /// The section must not describe the built-in rules. With no overlay
    /// installed it says nothing about any pack at all - no file names, no
    /// per-pack state, no "not present" placeholder.
    ///
    /// Asserted by the absence of both file names, because those are what a
    /// pack line is built from: a section that listed the packs could not
    /// pass this, and neither could one that reported their state.
    #[test]
    fn nothing_is_said_about_a_pack_when_no_overlay_is_installed() {
        let test = open_rules();
        let s = test.strings();

        // Guard the guard: if an overlay ever did sit next to the test
        // binary this test would be asserting nothing, so say so instead of
        // passing quietly.
        let installed = [
            crate::worker::RULES_FILE_NAME,
            crate::worker::L10N_RULES_FILE_NAME,
        ]
        .into_iter()
        .filter(|name| {
            crate::worker::rules_io::pack_is_present(match *name {
                n if n == crate::worker::RULES_FILE_NAME => {
                    gametrimmer_core::packs::PackKind::CategoryRules
                }
                _ => gametrimmer_core::packs::PackKind::LangPack,
            })
        })
        .count();
        assert_eq!(
            installed, 0,
            "an overlay is sitting next to the test binary; this test cannot judge",
        );

        assert_eq!(test.count_labels(s.rules_found_label), 0);
        assert_eq!(test.count_labels(crate::worker::RULES_FILE_NAME), 0);
        assert_eq!(test.count_labels(crate::worker::L10N_RULES_FILE_NAME), 0);
        assert_eq!(test.count_labels(s.rules_valid_label), 0);
        assert_eq!(test.count_labels(s.rules_invalid_label), 0);
    }
}
