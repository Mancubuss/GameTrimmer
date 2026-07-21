//! UI rendering, split by screen region. Each submodule takes `&mut
//! GameTrimmerApp` plus the enclosing `egui::Ui` and renders its panel.

pub mod bottom_bar;
pub mod dialogs;
pub mod libraries_panel;
pub mod row_actions;
pub mod settings_dialog;
pub mod top_bar;
pub mod tree_view;
