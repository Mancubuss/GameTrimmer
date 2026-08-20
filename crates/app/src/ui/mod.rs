//! UI rendering, split by screen region. Each submodule takes `&mut
//! GameTrimmerApp` plus the enclosing `egui::Ui` and renders its panel.

pub mod bottom_bar;
pub mod dialogs;
#[cfg(test)]
pub mod harness;
pub mod highlight;
pub mod onboarding;
pub mod plan_panel;
pub mod row_actions;
pub mod settings;
pub mod top_bar;
pub mod tree_view;

use eframe::egui;

use crate::i18n;
use crate::model::{self, FindingItem, UiAggregates};

#[derive(Clone)]
struct FrameUiAggregates {
    frame: u64,
    value: UiAggregates,
}

/// Returns the one findings roll-up shared by every summary surface in this
/// egui frame. The bottom bar is rendered before the central tree/plan row,
/// so it populates the cache and the plan row reuses it instead of launching
/// two more whole-findings scans. The frame number is the invalidation
/// boundary: any selection/removal mutation is visible on the next repaint.
pub(crate) fn frame_ui_aggregates(ctx: &egui::Context, findings: &[FindingItem]) -> UiAggregates {
    let frame = ctx.cumulative_frame_nr();
    let cache_id = egui::Id::new("gametrimmer.frame_ui_aggregates");
    if let Some(cached) = ctx.data(|data| data.get_temp::<FrameUiAggregates>(cache_id)) {
        if cached.frame == frame {
            return cached.value;
        }
    }

    let value = model::ui_aggregates(findings);
    ctx.data_mut(|data| {
        data.insert_temp(
            cache_id,
            FrameUiAggregates {
                frame,
                value: value.clone(),
            },
        );
    });
    value
}

/// Who this is built on. Small text, an acknowledgement rather than a banner.
///
/// Shared rather than copied because it is drawn in two places now, and they
/// have to stay the same list: the first-run screen, where most users read it
/// once, and "General" in the settings, which is where it can still be found
/// afterwards. The first-run screen retires itself for good after one scan,
/// so without the second copy a thank-you becomes unreachable - and a
/// thank-you nobody can find is one written on the assumption that nobody
/// will look.
pub fn credits(ui: &mut egui::Ui, s: &i18n::Strings) {
    ui.strong(s.credits_heading);
    ui.add_space(6.0);
    for line in [s.credits_anthropic, s.credits_karpathy, s.credits_tikione] {
        ui.small(line);
        ui.add_space(4.0);
    }
}

/// A button that says why it is unavailable.
///
/// `blocked_by` is `None` when the action can run, or the reason it cannot -
/// which becomes the hover text on the greyed-out button. A disabled control
/// with no explanation reads as broken rather than as gated, and every
/// action in the top and bottom bars is gated on something (a running job,
/// no scan results yet, an empty selection).
///
/// Callers pass the *first* applicable reason, so the hover text names the
/// blocker the user has to clear first rather than listing all of them.
pub fn gated_button(
    ui: &mut egui::Ui,
    label: &str,
    blocked_by: Option<&'static str>,
) -> egui::Response {
    let response = ui.add_enabled(blocked_by.is_none(), egui::Button::new(label));
    match blocked_by {
        Some(reason) => response.on_disabled_hover_text(reason),
        None => response,
    }
}

/// A red-framed block under a danger heading.
///
/// Danger is a *treatment*, not a place (protected-language editing): the control that earns the
/// frame is the one that has to sit next to the rest of its own setting, and
/// moving it into a single global "danger" section would split one mental
/// model across two screens. So the frame travels to the control instead.
///
/// Shared rather than copied because red only works while it is scarce - the
/// frame has to mean the same thing everywhere one is drawn, and there has to
/// be exactly one style of it to keep that true. It lives here rather than in
/// `ui::settings` because the first-run screen draws one too, around the
/// liability disclaimer.
pub fn danger_frame(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.5, ui.visuals().error_fg_color))
        .inner_margin(egui::Margin::same(10))
        .corner_radius(4)
        .show(ui, |ui| {
            ui.colored_label(ui.visuals().error_fg_color, label);
            ui.add_space(6.0);
            contents(ui);
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FindingRow, FindingSource};
    use gametrimmer_core::models::FindingAction;
    use gametrimmer_core::rules::Category;
    use std::path::PathBuf;

    #[test]
    fn frame_aggregate_cache_reuses_then_invalidates_on_the_next_frame() {
        let ctx = egui::Context::default();
        let mut findings = vec![FindingItem {
            row: FindingRow {
                file_id: 1,
                game_id: 1,
                game_name: "Game".to_string(),
                app_id: None,
                install_dir: PathBuf::from("C:/Game"),
                rel_path: "bonus.bin".to_string(),
                size: 100,
                size_on_disk: 40,
                source: FindingSource::Rule(Category::Bonus),
                rule_desc: String::new(),
                confidence: 80,
                lang_tag: None,
                group_dir: None,
                deletion_block_reason: None,
                imported_untrusted: false,
                library: None,
                action: FindingAction::DirectDelete,
                anti_cheat_protected: false,
                monolith_badge: None,
            },
            selected: false,
            removed: false,
        }];

        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert_eq!(frame_ui_aggregates(ui.ctx(), &findings).selected_count, 0);
            findings[0].selected = true;
            assert_eq!(
                frame_ui_aggregates(ui.ctx(), &findings).selected_count,
                0,
                "both summary surfaces reuse the same frame snapshot"
            );
        });
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert_eq!(
                frame_ui_aggregates(ui.ctx(), &findings).selected_count,
                1,
                "the next frame invalidates after a selection mutation"
            );
        });
    }
}
