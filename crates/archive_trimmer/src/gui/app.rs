//! Main Application State & egui UI Implementation for Archive Trimmer.
//!
//! Provides an interactive desktop interface for scanning SQLite database candidates,
//! inspecting monolithic game archives and visualizing conservative potential savings.

use eframe::egui::{self, Color32, RichText, ScrollArea};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use crate::cli::format_bytes;
use crate::db_reader::{self, find_default_db_path, GameArchiveCandidates};
use crate::formats::ArchiveType;
use crate::gui::details_modal::{ArchiveDetailsModal, DetailsModalAction};
use crate::gui::worker::{create_worker_channel, spawn_scan_candidates, GameScanResult, WorkerMsg};
use crate::logger;

/// The main Archive Trimmer application state implementing `eframe::App`.
pub struct ArchiveTrimmerApp {
    pub db_path: Option<PathBuf>,
    pub db_path_input: String,
    pub candidates: Vec<GameArchiveCandidates>,
    pub games: Vec<GameScanResult>,
    pub selected_game_id: Option<i64>,

    // Search and filters
    pub search_query: String,
    pub type_filter: Option<ArchiveType>,
    pub only_trimmable: bool,
    pub only_safe: bool,

    // Language settings
    pub keep_languages_input: String,

    // Worker state
    pub is_scanning: bool,
    pub scan_progress: Option<(usize, usize, String)>,
    pub status_message: String,
    pub log_messages: Vec<String>,
    pub show_log_panel: bool,

    // Modals & Dialogs
    pub details_modal: Option<ArchiveDetailsModal>,

    // Worker channels
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
}

impl ArchiveTrimmerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>, initial_db_path: Option<PathBuf>) -> Self {
        let (tx, rx) = create_worker_channel();
        let db_path = initial_db_path.or_else(find_default_db_path);
        let db_path_input = db_path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        logger::init_log_path(None);

        let mut app = Self {
            db_path,
            db_path_input,
            candidates: Vec::new(),
            games: Vec::new(),
            selected_game_id: None,
            search_query: String::new(),
            type_filter: None,
            only_trimmable: false,
            only_safe: false,
            keep_languages_input: "english, sfx, common".to_string(),
            is_scanning: false,
            scan_progress: None,
            status_message: "Ready. Select gametrimmer.db and click 'Scan DB Archives'."
                .to_string(),
            log_messages: Vec::new(),
            show_log_panel: false,
            details_modal: None,
            tx,
            rx,
        };

        app.push_log("INFO", "Archive Trimmer application initialized.");
        if let Some(ref path) = app.db_path {
            app.push_log("INFO", &format!("Database path detected: {:?}", path));
        }

        // If a default DB exists, trigger an automatic scan start
        if app.db_path.is_some() {
            app.start_db_scan(_cc.egui_ctx.clone());
        }

        app
    }

    /// Logs a formatted message in real-time to memory and to `archive-trimmer.log` on disk.
    pub fn push_log(&mut self, level: &str, message: &str) {
        let entry = logger::log_entry(level, message);
        self.log_messages.push(entry);
    }

    /// Starts scanning candidate archives from the selected database.
    pub fn start_db_scan(&mut self, ctx: egui::Context) {
        if self.is_scanning {
            return;
        }

        let Some(db_path) = self.db_path.clone() else {
            self.status_message =
                "Error: Please select a valid gametrimmer.db file first.".to_string();
            self.push_log("ERROR", "Cannot scan: database path is not set.");
            return;
        };

        self.push_log("INFO", &format!("Reading database at {:?}", db_path));

        match db_reader::read_games_with_candidates(&db_path) {
            Ok(candidates) => {
                if candidates.is_empty() {
                    self.status_message =
                        "Database read successfully, but no matching candidate archives were found."
                            .to_string();
                    self.push_log(
                        "WARN",
                        "No candidate monolithic archives found in database.",
                    );
                    self.candidates.clear();
                    self.games.clear();
                    return;
                }

                self.candidates = candidates.clone();
                self.games.clear();
                self.selected_game_id = None;
                self.is_scanning = true;
                self.scan_progress = Some((0, candidates.len(), "Starting scan...".to_string()));
                self.status_message =
                    format!("Scanning {} games from database...", candidates.len());
                self.push_log(
                    "INFO",
                    &format!(
                        "Discovered {} games with candidate archives. Spawning scan worker.",
                        candidates.len()
                    ),
                );

                let tx = self.tx.clone();
                spawn_scan_candidates(candidates, tx, ctx);
            }
            Err(err) => {
                self.status_message = format!("Failed to read database: {err}");
                self.push_log("ERROR", &format!("DB Read Error: {err}"));
            }
        }
    }

    /// Drains messages received from background worker threads.
    fn drain_worker_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                WorkerMsg::ScanProgress {
                    current_game,
                    total_games,
                    game_name,
                } => {
                    self.scan_progress = Some((current_game, total_games, game_name.clone()));
                    self.status_message =
                        format!("Analyzing ({current_game}/{total_games}): {game_name}");
                }
                WorkerMsg::GameScanned { game_result } => {
                    let game_result = *game_result;
                    if !game_result.archives.is_empty() {
                        // Update or insert game result
                        if let Some(existing) = self
                            .games
                            .iter_mut()
                            .find(|g| g.game_id == game_result.game_id)
                        {
                            *existing = game_result;
                        } else {
                            // Auto-select first game if none selected
                            if self.selected_game_id.is_none() {
                                self.selected_game_id = Some(game_result.game_id);
                            }
                            self.games.push(game_result);
                        }
                    }
                }
                WorkerMsg::ScanComplete {
                    total_savings,
                    total_archives,
                    total_games_with_monoliths,
                } => {
                    self.is_scanning = false;
                    self.scan_progress = None;
                    self.status_message = format!(
                        "Scan complete! Discovered {} archives across {} games ({} potential savings).",
                        total_archives,
                        total_games_with_monoliths,
                        format_bytes(total_savings)
                    );
                    self.push_log(
                        "INFO",
                        &format!(
                            "Scan finished: {} archives across {} games, {} potential savings",
                            total_archives,
                            total_games_with_monoliths,
                            format_bytes(total_savings)
                        ),
                    );
                }
                WorkerMsg::Log { message } => {
                    if let Some(rest) = message.strip_prefix("[WARN] ") {
                        self.push_log("WARN", rest);
                    } else if let Some(rest) = message.strip_prefix("[ERROR] ") {
                        self.push_log("ERROR", rest);
                    } else if let Some(rest) = message.strip_prefix("[INFO] ") {
                        self.push_log("INFO", rest);
                    } else {
                        self.push_log("INFO", &message);
                    }
                }
                WorkerMsg::Error { message } => {
                    self.status_message = format!("Error: {message}");
                    self.push_log("ERROR", &message);
                }
            }
        }
    }
}

impl eframe::App for ArchiveTrimmerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drain_worker_messages();

        // 1. Top Header Panel
        egui::Panel::top("top_header").show(ui, |ui| {
            self.render_header(ui, &ctx);
        });

        // 2. Bottom Footer Panel
        egui::Panel::bottom("bottom_footer").show(ui, |ui| {
            self.render_footer(ui);
        });

        // 3. Central Split View
        egui::CentralPanel::default().show(ui, |ui| {
            self.render_central_split(ui, &ctx);
        });

        // 4. Modals & Dialogs
        self.render_modals(&ctx);
    }
}

struct GameSummaryItem {
    game_id: i64,
    game_name: String,
    is_safe: bool,
    engine_name: Option<String>,
    archives_count: usize,
    total_trimmable_bytes: u64,
}

impl ArchiveTrimmerApp {
    /// Renders the top toolbar, database selector, filters, and summary statistics.
    fn render_header(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(4.0);

        // Row 1: DB Path + Action Buttons
        ui.horizontal(|ui| {
            ui.label(RichText::new("SQLite DB:").strong());

            let input_width = (ui.available_width() - 320.0).max(120.0);
            let text_edit = ui.add_sized(
                [input_width, 22.0],
                egui::TextEdit::singleline(&mut self.db_path_input)
                    .hint_text("Path to gametrimmer.db..."),
            );

            if text_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let p = PathBuf::from(&self.db_path_input);
                if p.exists() {
                    self.db_path = Some(p);
                    self.start_db_scan(ctx.clone());
                }
            }

            if ui.button("📁 Browse DB...").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("GameTrimmer Database", &["db", "sqlite"])
                    .pick_file()
                {
                    self.db_path_input = path.to_string_lossy().into_owned();
                    self.db_path = Some(path);
                    self.start_db_scan(ctx.clone());
                }
            }

            if self.is_scanning {
                ui.add(egui::Spinner::new());
                ui.label(RichText::new("Scanning...").color(Color32::from_rgb(241, 196, 15)));
            } else {
                let scan_btn = ui.add_enabled(
                    self.db_path.is_some(),
                    egui::Button::new(
                        RichText::new("🔄 Scan DB Archives")
                            .strong()
                            .color(Color32::WHITE),
                    ),
                );
                if scan_btn.clicked() {
                    self.start_db_scan(ctx.clone());
                }
            }
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(2.0);

        // Row 2: Search Query + Archive Type Filter + Keep Languages Input
        ui.horizontal(|ui| {
            ui.label("Search:");
            ui.add_sized(
                [180.0, 20.0],
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("Game or archive name..."),
            );
            if !self.search_query.is_empty() && ui.button("✖").clicked() {
                self.search_query.clear();
            }

            ui.separator();

            ui.label("Format:");
            egui::ComboBox::from_id_salt("header_type_filter")
                .selected_text(
                    self.type_filter
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "All Formats".to_string()),
                )
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.type_filter, None, "All Formats");
                    ui.selectable_value(
                        &mut self.type_filter,
                        Some(ArchiveType::WwisePck),
                        "Wwise PCK",
                    );
                    ui.selectable_value(
                        &mut self.type_filter,
                        Some(ArchiveType::WwiseBnk),
                        "Wwise BNK",
                    );
                    ui.selectable_value(
                        &mut self.type_filter,
                        Some(ArchiveType::UnrealPak),
                        "Unreal PAK",
                    );
                    ui.selectable_value(
                        &mut self.type_filter,
                        Some(ArchiveType::CapcomRePak),
                        "Capcom RE PAK",
                    );
                    ui.selectable_value(
                        &mut self.type_filter,
                        Some(ArchiveType::ElectronAsar),
                        "Electron ASAR",
                    );
                    ui.selectable_value(
                        &mut self.type_filter,
                        Some(ArchiveType::UnityAssetBundle),
                        "Unity AssetBundle",
                    );
                    ui.selectable_value(
                        &mut self.type_filter,
                        Some(ArchiveType::BinkVideo),
                        "Bink Video",
                    );
                });

            ui.separator();

            ui.label("Keep Languages:");
            ui.add_sized(
                [160.0, 20.0],
                egui::TextEdit::singleline(&mut self.keep_languages_input),
            );

            ui.checkbox(&mut self.only_trimmable, "Trimmable Only");
            ui.checkbox(&mut self.only_safe, "Safe (No AC) Only");
        });

        ui.add_space(4.0);

        // Row 3: Aggregate Statistics Cards
        self.render_stats_banner(ui);
        ui.add_space(4.0);
    }

    /// Renders statistical badges at top of screen.
    fn render_stats_banner(&self, ui: &mut egui::Ui) {
        let total_games = self.games.len();
        let total_archives: usize = self.games.iter().map(|g| g.archives.len()).sum();
        let total_logical_size: u64 = self.games.iter().map(|g| g.total_logical_size).sum();
        let total_potential_savings: u64 = self.games.iter().map(|g| g.total_trimmable_bytes).sum();
        let safe_games = self.games.iter().filter(|g| g.is_safe).count();
        let protected_games = total_games.saturating_sub(safe_games);

        ui.horizontal(|ui| {
            ui.group(|ui| {
                ui.label(format!("🎮 Games: {total_games}"));
                ui.separator();
                ui.label(format!("📦 Monoliths: {total_archives}"));
                ui.separator();
                ui.label(format!("💾 Size: {}", format_bytes(total_logical_size)));
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "⚡ Potential Savings: {}",
                        format_bytes(total_potential_savings)
                    ))
                    .color(Color32::from_rgb(46, 204, 113))
                    .strong(),
                );
                ui.separator();
                if protected_games > 0 {
                    ui.label(
                        RichText::new(format!("🛡️ Anti-Cheat Protected: {protected_games}"))
                            .color(Color32::from_rgb(231, 76, 60)),
                    );
                } else {
                    ui.label(RichText::new("🛡️ All Safe").color(Color32::from_rgb(46, 204, 113)));
                }
            });
        });
    }

    /// Renders the central split view (left list of games, right details of selected game).
    fn render_central_split(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let search_lower = self.search_query.to_lowercase();

        // Collect lightweight summary list for left panel
        let filtered_games: Vec<GameSummaryItem> = self
            .games
            .iter()
            .filter(|g| {
                if g.archives.is_empty() {
                    return false;
                }
                if self.only_safe && !g.is_safe {
                    return false;
                }
                if self.only_trimmable && g.total_trimmable_bytes == 0 {
                    return false;
                }
                if let Some(target_type) = self.type_filter {
                    if !g
                        .archives
                        .iter()
                        .any(|a| a.archive_type == Some(target_type))
                    {
                        return false;
                    }
                }
                if !search_lower.is_empty() {
                    let name_match = g.game_name.to_lowercase().contains(&search_lower);
                    let arch_match = g
                        .archives
                        .iter()
                        .any(|a| a.rel_path.to_lowercase().contains(&search_lower));
                    if !name_match && !arch_match {
                        return false;
                    }
                }
                true
            })
            .map(|g| GameSummaryItem {
                game_id: g.game_id,
                game_name: g.game_name.clone(),
                is_safe: g.is_safe,
                engine_name: g
                    .safety_report
                    .findings
                    .first()
                    .map(|f| f.engine.to_string()),
                archives_count: g.archives.len(),
                total_trimmable_bytes: g.total_trimmable_bytes,
            })
            .collect();

        // Left Panel (Games list)
        egui::Panel::left("left_games_panel")
            .resizable(true)
            .default_size(360.0)
            .size_range(260.0..=600.0)
            .show(ui, |ui| {
                ui.heading("Detected Games");
                ui.label(
                    RichText::new(format!("{} games matching filter", filtered_games.len()))
                        .small()
                        .color(Color32::GRAY),
                );
                ui.separator();

                ScrollArea::vertical()
                    .id_salt("games_scroll_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if filtered_games.is_empty() {
                            ui.add_space(20.0);
                            ui.label("No games match the current filter.");
                            return;
                        }

                        for game in &filtered_games {
                            let is_selected = self.selected_game_id == Some(game.game_id);

                            ui.group(|ui| {
                                let resp = ui.selectable_label(
                                    is_selected,
                                    RichText::new(&game.game_name).strong(),
                                );

                                if resp.clicked() {
                                    self.selected_game_id = Some(game.game_id);
                                }

                                ui.horizontal(|ui| {
                                    // Anti-Cheat badge
                                    if game.is_safe {
                                        ui.label(
                                            RichText::new("🛡️ Safe")
                                                .small()
                                                .color(Color32::from_rgb(46, 204, 113)),
                                        );
                                    } else {
                                        let engine =
                                            game.engine_name.as_deref().unwrap_or("Anti-Cheat");
                                        ui.label(
                                            RichText::new(format!("⚠️ {engine}"))
                                                .small()
                                                .color(Color32::from_rgb(231, 76, 60)),
                                        );
                                    }

                                    ui.separator();
                                    ui.label(
                                        RichText::new(format!("{} arch", game.archives_count))
                                            .small(),
                                    );

                                    if game.total_trimmable_bytes > 0 {
                                        ui.separator();
                                        ui.label(
                                            RichText::new(format_bytes(game.total_trimmable_bytes))
                                                .small()
                                                .color(Color32::from_rgb(46, 204, 113))
                                                .strong(),
                                        );
                                    }
                                });
                            });
                            ui.add_space(2.0);
                        }
                    });
            });

        // Right Panel (Game Details & Archives Table)
        egui::CentralPanel::default().show(ui, |ui| {
            self.render_game_details(ui, ctx);
        });
    }

    /// Renders the selected game's archive details and per-archive action buttons.
    fn render_game_details(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let selected_game = self
            .games
            .iter()
            .find(|g| Some(g.game_id) == self.selected_game_id)
            .cloned();

        let Some(game) = selected_game else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    RichText::new(
                        "👈 Select a game from the left panel to inspect its monolithic archives.",
                    )
                    .italics()
                    .color(Color32::GRAY),
                );
            });
            return;
        };

        // Header Box
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.heading(&game.game_name);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if game.is_safe {
                        ui.label(
                            RichText::new("🛡️ Safe (No Anti-Cheat Detected)")
                                .color(Color32::from_rgb(46, 204, 113))
                                .strong(),
                        );
                    } else {
                        ui.label(
                            RichText::new("⚠️ Anti-Cheat Protected")
                                .color(Color32::from_rgb(231, 76, 60))
                                .strong(),
                        );
                    }
                });
            });

            ui.horizontal(|ui| {
                ui.label(RichText::new("Install Directory:").strong());
                ui.label(
                    RichText::new(game.game_root.to_string_lossy())
                        .monospace()
                        .color(Color32::from_rgb(180, 210, 255)),
                );
            });

            if !game.is_safe {
                for finding in &game.safety_report.findings {
                    ui.label(
                        RichText::new(format!("⚠️ {}: {}", finding.engine, finding.warning))
                            .color(Color32::from_rgb(255, 160, 120))
                            .small(),
                    );
                }
            }

            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Total Monoliths: {}", game.archives.len()));
                ui.separator();
                ui.label(format!(
                    "Logical Size: {}",
                    format_bytes(game.total_logical_size)
                ));
                ui.separator();
                ui.label(format!(
                    "On-Disk: {}",
                    format_bytes(game.total_on_disk_size)
                ));
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "Potential Savings: {}",
                        format_bytes(game.total_trimmable_bytes)
                    ))
                    .color(Color32::from_rgb(46, 204, 113))
                    .strong(),
                );
                if !game.detected_languages.is_empty() {
                    ui.separator();
                    ui.label(format!("Languages: {}", game.detected_languages.join(", ")));
                }
            });

            ui.add_space(4.0);

            // Per-Game Batch Action Toolbar
            ui.horizontal(|ui| {
                ui.add_enabled(
                    false,
                    egui::Button::new(RichText::new("Batch Mutation Disabled").strong()),
                )
                .on_hover_text("Disabled until full payload rollback is available");

                ui.add_enabled(
                    false,
                    egui::Button::new(
                        RichText::new("Live Mutation Disabled")
                            .strong()
                            .color(Color32::from_rgb(255, 120, 120)),
                    ),
                )
                .on_hover_text("Disabled until full payload rollback is available");
            });
        });

        ui.add_space(6.0);

        // Archives List for this game
        ui.heading("Monolithic Archives in this Game");

        let archives = game.archives.clone();

        ScrollArea::both()
            .id_salt("game_archives_table")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if archives.is_empty() {
                    ui.label("No recognized monolithic archives detected in this game.");
                    return;
                }

                egui::Grid::new("archives_grid")
                    .striped(true)
                    .num_columns(6)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        // Header
                        ui.label(RichText::new("Archive File").strong());
                        ui.label(RichText::new("Format").strong());
                        ui.label(RichText::new("Size / On-Disk").strong());
                        ui.label(RichText::new("Potential Savings").strong());
                        ui.label(RichText::new("Detected Languages").strong());
                        ui.label(RichText::new("Actions").strong());
                        ui.end_row();

                        for arch in &archives {
                            // Column 1: Rel Path
                            ui.label(
                                RichText::new(&arch.rel_path)
                                    .monospace()
                                    .color(Color32::WHITE),
                            );

                            // Column 2: Format Badge
                            if let Some(atype) = arch.archive_type {
                                ui.label(
                                    RichText::new(format!("[{atype}]"))
                                        .color(Color32::from_rgb(100, 200, 255)),
                                );
                            } else {
                                ui.label(RichText::new("[Unknown]").color(Color32::GRAY));
                            }

                            // Column 3: Size
                            ui.label(format!(
                                "{} ({})",
                                format_bytes(arch.size),
                                format_bytes(arch.on_disk_size)
                            ));

                            // Column 4: Potential Savings
                            let trimmable = arch
                                .analysis
                                .as_ref()
                                .map(|a| a.total_trimmable_bytes)
                                .unwrap_or(0);
                            if trimmable > 0 {
                                ui.label(
                                    RichText::new(format_bytes(trimmable))
                                        .color(Color32::from_rgb(46, 204, 113))
                                        .strong(),
                                );
                            } else {
                                ui.label(RichText::new("0 B").color(Color32::GRAY));
                            }

                            // Column 5: Languages
                            let langs = arch
                                .analysis
                                .as_ref()
                                .map(|a| a.detected_languages.join(", "))
                                .unwrap_or_default();
                            if !langs.is_empty() {
                                ui.label(
                                    RichText::new(langs).color(Color32::from_rgb(241, 196, 15)),
                                );
                            } else {
                                ui.label(RichText::new("-").color(Color32::DARK_GRAY));
                            }

                            // Column 6: Actions
                            ui.horizontal(|ui| {
                                if let Some(ref analysis) = arch.analysis {
                                    if ui.button("🔍 Details").clicked() {
                                        self.details_modal =
                                            Some(ArchiveDetailsModal::new(analysis.clone()));
                                    }
                                }

                                ui.add_enabled(false, egui::Button::new("Mutation Disabled"))
                                    .on_hover_text(
                                        "Disabled until full payload rollback is available",
                                    );

                                ui.add_enabled(
                                    false,
                                    egui::Button::new(
                                        RichText::new("Live Disabled")
                                            .color(Color32::from_rgb(255, 120, 120)),
                                    ),
                                )
                                .on_hover_text("Disabled until full payload rollback is available");
                            });

                            ui.end_row();
                        }
                    });
            });
    }

    /// Renders bottom status bar, progress bar, and log message ticker.
    fn render_footer(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);

        if let Some((curr, total, ref name)) = self.scan_progress {
            let frac = if total > 0 {
                curr as f32 / total as f32
            } else {
                0.0
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .text(format!("Scanning ({curr}/{total}): {name}"))
                    .animate(true),
            );
        }

        ui.horizontal(|ui| {
            ui.label(RichText::new("Status:").strong());
            ui.label(RichText::new(&self.status_message).color(Color32::from_rgb(220, 220, 220)));

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(if self.show_log_panel {
                        "Hide Activity Log"
                    } else {
                        "Show Activity Log"
                    })
                    .clicked()
                {
                    self.show_log_panel = !self.show_log_panel;
                }

                if ui
                    .button("📄 Open Log File")
                    .on_hover_text("Open archive-trimmer.log in default text editor")
                    .clicked()
                {
                    if let Err(err) = logger::open_log_file() {
                        self.status_message = format!("Failed to open log: {err}");
                        self.push_log("ERROR", &format!("Failed to open log: {err}"));
                    }
                }

                if ui
                    .button("🗑️ Clear Log")
                    .on_hover_text("Clear in-memory and on-disk activity logs")
                    .clicked()
                {
                    self.log_messages.clear();
                    let _ = logger::clear_log_file();
                    self.status_message = "Activity log cleared.".to_string();
                }
            });
        });

        // Expandable Activity Log
        if self.show_log_panel {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(
                        "Recent Activity Log (streaming in real-time to archive-trimmer.log):",
                    )
                    .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("📄 Open File").clicked() {
                        let _ = logger::open_log_file();
                    }
                    if ui.button("🗑️ Clear").clicked() {
                        self.log_messages.clear();
                        let _ = logger::clear_log_file();
                    }
                });
            });
            ScrollArea::vertical()
                .max_height(140.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if self.log_messages.is_empty() {
                        ui.label(
                            RichText::new("No activity log entries recorded yet.")
                                .color(Color32::GRAY)
                                .italics(),
                        );
                    } else {
                        for line in &self.log_messages {
                            ui.label(RichText::new(line).monospace().small());
                        }
                    }
                });
        }

        ui.add_space(2.0);
    }

    /// Renders the read-only archive details modal.
    fn render_modals(&mut self, ctx: &egui::Context) {
        // 1. Details Modal
        if let Some(mut modal) = self.details_modal.take() {
            let action = modal.show(ctx);
            match action {
                Some(DetailsModalAction::Close) => {
                    self.details_modal = None;
                }
                Some(DetailsModalAction::TrimDryRun(_path)) => {
                    self.details_modal = Some(modal);
                    self.status_message =
                        "Archive mutation is disabled until full payload rollback is available."
                            .to_string();
                }
                Some(DetailsModalAction::TrimLive(_path)) => {
                    self.details_modal = Some(modal);
                    self.status_message =
                        "Archive mutation is disabled until full payload rollback is available."
                            .to_string();
                }
                None => {
                    self.details_modal = Some(modal);
                }
            }
        }
    }
}
