//! Archive Stream & Byte-Range Details Modal Inspector.
//!
//! Renders an interactive modal inspecting the internal stream structure,
//! byte offsets, language tags, and sparse-zeroable chunks of a monolithic archive.

use eframe::egui::{self, Color32, RichText, ScrollArea, Vec2};
use std::path::PathBuf;

use crate::cli::format_bytes;
use crate::formats::ArchiveAnalysis;

/// Action triggered from inside the details modal dialog.
pub enum DetailsModalAction {
    Close,
    TrimDryRun(PathBuf),
    TrimLive(PathBuf),
}

/// Modal dialog state for inspecting archive internals.
#[derive(Debug, Clone)]
pub struct ArchiveDetailsModal {
    pub analysis: ArchiveAnalysis,
    pub search_query: String,
    pub language_filter: Option<String>,
    pub show_only_trimmable: bool,
    pub is_open: bool,
}

impl ArchiveDetailsModal {
    pub fn new(analysis: ArchiveAnalysis) -> Self {
        Self {
            analysis,
            search_query: String::new(),
            language_filter: None,
            show_only_trimmable: false,
            is_open: true,
        }
    }

    /// Renders the modal window. Returns `Some(DetailsModalAction)` if an action was requested.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<DetailsModalAction> {
        if !self.is_open {
            return None;
        }

        let mut action = None;
        let mut is_open = self.is_open;

        egui::Window::new(format!("Archive Inspector: {}", self.analysis.archive_type))
            .open(&mut is_open)
            .collapsible(false)
            .resizable(true)
            .default_size(Vec2::new(850.0, 550.0))
            .min_size(Vec2::new(600.0, 400.0))
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                // Top Info Section
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Path:").strong());
                        ui.label(
                            RichText::new(self.analysis.path.to_string_lossy())
                                .monospace()
                                .color(Color32::from_rgb(180, 210, 255)),
                        );
                    });

                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(format!("Format: {}", self.analysis.archive_type))
                                .strong(),
                        );
                        ui.separator();
                        ui.label(format!(
                            "Logical Size: {}",
                            format_bytes(self.analysis.total_size)
                        ));
                        ui.separator();
                        ui.label(format!(
                            "On-Disk: {}",
                            format_bytes(self.analysis.on_disk_size)
                        ));
                        ui.separator();
                        ui.label(
                            RichText::new(format!(
                                "Potential Savings: {}",
                                format_bytes(self.analysis.total_trimmable_bytes)
                            ))
                            .color(Color32::from_rgb(46, 204, 113))
                            .strong(),
                        );
                    });

                    if !self.analysis.details.is_empty() {
                        ui.label(
                            RichText::new(format!("Details: {}", self.analysis.details))
                                .italics()
                                .color(Color32::GRAY),
                        );
                    }
                });

                ui.add_space(6.0);

                // Filters & Search Bar
                ui.horizontal(|ui| {
                    ui.label("Search Chunks:");
                    ui.text_edit_singleline(&mut self.search_query);

                    if !self.search_query.is_empty() && ui.button("✖").clicked() {
                        self.search_query.clear();
                    }

                    ui.separator();
                    ui.checkbox(&mut self.show_only_trimmable, "Trimmable Only");

                    if !self.analysis.detected_languages.is_empty() {
                        ui.separator();
                        ui.label("Language:");
                        egui::ComboBox::from_id_salt("modal_lang_filter")
                            .selected_text(
                                self.language_filter.as_deref().unwrap_or("All Languages"),
                            )
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.language_filter,
                                    None,
                                    "All Languages",
                                );
                                for lang in &self.analysis.detected_languages {
                                    ui.selectable_value(
                                        &mut self.language_filter,
                                        Some(lang.clone()),
                                        lang,
                                    );
                                }
                            });
                    }
                });

                ui.add_space(6.0);

                // Filter chunks
                let search_lower = self.search_query.to_lowercase();
                let filtered_chunks: Vec<_> = self
                    .analysis
                    .trimmable_chunks
                    .iter()
                    .enumerate()
                    .filter(|(_, chunk)| {
                        if self.show_only_trimmable && !chunk.can_zero_in_place {
                            return false;
                        }
                        if let Some(ref filter_lang) = self.language_filter {
                            if chunk.language.as_deref() != Some(filter_lang) {
                                return false;
                            }
                        }
                        if !search_lower.is_empty() {
                            let name_match = chunk.name.to_lowercase().contains(&search_lower);
                            let id_match = chunk.id.to_lowercase().contains(&search_lower);
                            let cat_match = chunk.category.to_lowercase().contains(&search_lower);
                            let lang_match = chunk
                                .language
                                .as_deref()
                                .map(|l| l.to_lowercase().contains(&search_lower))
                                .unwrap_or(false);
                            if !name_match && !id_match && !cat_match && !lang_match {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();

                ui.label(
                    RichText::new(format!(
                        "Displaying {} / {} chunks:",
                        filtered_chunks.len(),
                        self.analysis.trimmable_chunks.len()
                    ))
                    .small(),
                );

                // Chunk Table Scroll Area
                ScrollArea::vertical()
                    .max_height(300.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("chunks_grid")
                            .striped(true)
                            .num_columns(7)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                // Header
                                ui.label(RichText::new("#").strong());
                                ui.label(RichText::new("Name / Identifier").strong());
                                ui.label(RichText::new("Offset").strong());
                                ui.label(RichText::new("Length").strong());
                                ui.label(RichText::new("Language").strong());
                                ui.label(RichText::new("Category").strong());
                                ui.label(RichText::new("Zeroable?").strong());
                                ui.end_row();

                                for (idx, chunk) in filtered_chunks {
                                    ui.label(format!("{:03}", idx + 1));
                                    ui.label(
                                        RichText::new(&chunk.name)
                                            .monospace()
                                            .color(Color32::WHITE),
                                    );
                                    ui.label(
                                        RichText::new(format!("0x{:08X}", chunk.offset))
                                            .monospace()
                                            .color(Color32::from_rgb(200, 200, 200)),
                                    );
                                    ui.label(format_bytes(chunk.length));

                                    if let Some(ref lang) = chunk.language {
                                        ui.label(
                                            RichText::new(format!("[{lang}]"))
                                                .color(Color32::from_rgb(241, 196, 15)),
                                        );
                                    } else {
                                        ui.label(RichText::new("-").color(Color32::DARK_GRAY));
                                    }

                                    ui.label(&chunk.category);

                                    if chunk.can_zero_in_place {
                                        ui.label(
                                            RichText::new("Yes (Sparse)")
                                                .color(Color32::from_rgb(46, 204, 113)),
                                        );
                                    } else {
                                        ui.label(
                                            RichText::new("No")
                                                .color(Color32::from_rgb(150, 150, 150)),
                                        );
                                    }

                                    ui.end_row();
                                }
                            });
                    });

                ui.add_space(10.0);
                ui.separator();

                // Bottom Action Buttons
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            false,
                            egui::Button::new(RichText::new("Mutation Disabled").strong()),
                        )
                        .on_hover_text("Disabled until full payload rollback is available")
                        .clicked()
                    {
                        action = Some(DetailsModalAction::TrimDryRun(self.analysis.path.clone()));
                    }

                    if ui
                        .add_enabled(
                            false,
                            egui::Button::new(
                                RichText::new("⚡ In-Place Sparse Trim")
                                    .strong()
                                    .color(Color32::from_rgb(255, 120, 120)),
                            ),
                        )
                        .on_hover_text("Disabled until full payload rollback is available")
                        .clicked()
                    {
                        action = Some(DetailsModalAction::TrimLive(self.analysis.path.clone()));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            action = Some(DetailsModalAction::Close);
                        }
                    });
                });
            });

        if !is_open {
            self.is_open = false;
            action = Some(DetailsModalAction::Close);
        }

        action
    }
}
