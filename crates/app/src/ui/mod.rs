//! UI rendering, split by screen region. Each submodule takes `&mut
//! GameTrimmerApp` plus the enclosing `egui::Ui` and renders its panel.

pub mod bottom_bar;
pub mod dialogs;
#[cfg(test)]
pub mod harness;
pub mod libraries_panel;
pub mod plan_panel;
pub mod row_actions;
pub mod settings_dialog;
pub mod top_bar;
pub mod tree_view;

use eframe::egui;

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
