//! Central panel: the disk -> game -> category -> folder/file findings tree
//! with per-file checkboxes, aligned columns, and keyboard navigation.
//!
//! # Why virtualized rendering
//!
//! egui is immediate-mode: every widget on screen is laid out and painted
//! fresh every frame. Real user data can contain tens of thousands of
//! orphan file findings (localization findings rarely collapse into
//! folders, see `model::TreeNode::File`), so naively rendering one
//! collapsible header/grid row per node - as this file used to - means
//! laying out 10k-100k widgets every single frame, which freezes the UI.
//!
//! Instead, the whole tree is flattened each frame into a list of only the
//! rows that are *structurally* visible (i.e. not hidden inside a collapsed
//! disk/game/category/folder), and handed to
//! `egui::ScrollArea::show_rows`, which only asks us to actually lay out
//! the handful of rows currently scrolled into view. The flattening walk
//! itself is cheap: it produces a `Vec` of small `Copy` enum values (a few
//! indices each, no strings, no cloned `Vec<usize>`), so even at 100k rows
//! it's a fast, allocation-light pass - the expensive part (widget layout,
//! text shaping, formatting) only ever runs for the visible range.
//!
//! Disks and categories default to open; game and folder nodes default to
//! closed, so a fresh scan shows a compact per-game summary instead of a
//! wall of files - details stay one click (or `→`) away. The open/closed
//! state is tracked explicitly in `GameTrimmerApp::tree_toggles`, since the
//! id path a node would get under `show_rows` isn't stable in the way
//! egui's own `CollapsingState` memory needs.
//!
//! # Keyboard navigation
//!
//! When no widget has focus, the tree handles: `↑`/`↓` (move the cursor),
//! `PgUp`/`PgDn` (page; plain scrolling when no cursor is active),
//! `Home`/`End`, `→`/`←` (expand/collapse; `←` on a collapsed node jumps to
//! its parent), and `Space`/`Enter` (toggle selection of the cursor row).
//! The cursor row is highlighted; clicking a row's name places the cursor.

use std::collections::HashMap;

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n::{self, Lang};
use crate::model::{
    category_display, category_ui_key, format_size, group_min_confidence, group_selection_state,
    set_group_selection, toggle_group, DiskGroup, DisplayCategory, FindingItem, TreeNode,
    AUTO_SELECT_CONFIDENCE_THRESHOLD,
};

/// Horizontal indent per nesting level (disk = 0, game = 1, category = 2,
/// folder = 3, orphan file = 3, folder member = 4).
const INDENT_PX: f32 = 18.0;
/// Fixed column widths (right-aligned block), so every row lines up into
/// the same columns as the header row above the list.
const LANG_COLUMN_PX: f32 = 48.0;
const COUNT_COLUMN_PX: f32 = 64.0;
const SIZE_COLUMN_PX: f32 = 92.0;
const CONFIDENCE_COLUMN_PX: f32 = 72.0;

/// One visible row in the flattened tree, referencing the source tree only
/// by index - cheap to push into a per-frame `Vec` for tens of thousands of
/// rows.
#[derive(Debug, Clone, Copy)]
enum Row {
    Disk {
        d: usize,
    },
    Game {
        d: usize,
        g: usize,
    },
    Category {
        d: usize,
        g: usize,
        c: usize,
    },
    Folder {
        d: usize,
        g: usize,
        c: usize,
        n: usize,
    },
    /// `member = Some(i)` for the `i`-th member of a `TreeNode::Folder`;
    /// `member = None` for a standalone `TreeNode::File`.
    File {
        d: usize,
        g: usize,
        c: usize,
        n: usize,
        member: Option<usize>,
    },
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();
    egui::CentralPanel::default().show(ui, |ui| {
        if app.tree.is_empty() {
            let s = i18n::strings(lang);
            ui.add_space(16.0);
            ui.label(if app.busy {
                s.scanning_in_progress
            } else {
                s.no_findings_hint
            });
            return;
        }

        show_column_headers(ui, lang);
        ui.separator();

        let row_height = ui.spacing().interact_size.y;
        let row_stride = row_height + ui.spacing().item_spacing.y;

        // Keyboard input is handled before rendering so the resulting
        // cursor/toggle/scroll changes are visible the same frame. Modals
        // own the keyboard while open.
        let modal_open = app.confirm_delete.is_some()
            || app.remove_summary.is_some()
            || app.show_elevation_prompt;
        let mut scroll_override = None;
        if !modal_open {
            let rows = build_visible_rows(&app.tree, &app.tree_toggles);
            scroll_override = handle_keyboard(app, ui, &rows, row_stride);
        }

        // Rebuilt after key handling: expanding/collapsing above changes
        // which rows are visible.
        let rows = build_visible_rows(&app.tree, &app.tree_toggles);
        if let Some(cursor) = app.tree_cursor {
            if cursor >= rows.len() {
                app.tree_cursor = rows.len().checked_sub(1);
            }
        }

        // Disjoint field borrows: `tree` (read-only tree shape), `findings`
        // (checkbox mutation), `tree_toggles` (expand/collapse state) and
        // `tree_cursor` never alias each other, so this compiles without
        // cloning the tree.
        let tree = &app.tree;
        let findings = &mut app.findings;
        let toggles = &mut app.tree_toggles;
        let cursor = &mut app.tree_cursor;

        let mut scroll_area = egui::ScrollArea::vertical();
        if let Some(offset) = scroll_override {
            scroll_area = scroll_area.vertical_scroll_offset(offset.max(0.0));
        }
        let output = scroll_area.show_rows(ui, row_height, rows.len(), |ui, range| {
            for row_index in range {
                show_row(
                    ui,
                    tree,
                    findings,
                    toggles,
                    cursor,
                    rows[row_index],
                    row_index,
                    lang,
                );
            }
        });

        // Remembered for next frame's keyboard handling (PgUp/PgDn page
        // size, keeping the cursor scrolled into view).
        app.tree_scroll_offset = output.state.offset.y;
        app.tree_viewport_height = output.inner_rect.height();
    });
}

/// The header row naming the fixed columns, laid out with the same widths
/// as every data row so the columns visually line up.
fn show_column_headers(ui: &mut egui::Ui, lang: Lang) {
    let s = i18n::strings(lang);
    row_columns(
        ui,
        egui::RichText::new(s.col_language).strong(),
        egui::RichText::new(s.col_files).strong(),
        egui::RichText::new(s.col_size).strong(),
        egui::RichText::new(s.col_confidence).strong(),
        |ui| {
            ui.add_space(4.0);
            ui.strong(s.col_name);
        },
    );
}

/// Lays out one row as [flexible left part | Мова | Файлів | Розмір |
/// Довіра], with the four fixed-width columns right-aligned against the
/// panel edge. Every row (headers and files alike) goes through this, which
/// is what keeps the table aligned without a real grid widget.
fn row_columns(
    ui: &mut egui::Ui,
    lang: egui::RichText,
    count: egui::RichText,
    size: egui::RichText,
    confidence: egui::RichText,
    left: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            right_cell(ui, CONFIDENCE_COLUMN_PX, confidence);
            right_cell(ui, SIZE_COLUMN_PX, size);
            right_cell(ui, COUNT_COLUMN_PX, count);
            right_cell(ui, LANG_COLUMN_PX, lang);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), left);
        });
    });
}

/// One fixed-width, right-aligned cell of [`row_columns`].
fn right_cell(ui: &mut egui::Ui, width: f32, text: egui::RichText) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            ui.add(egui::Label::new(text).truncate());
        },
    );
}

/// The confidence-column text for a group of findings: empty when every
/// member is at or above the auto-select threshold (nothing to double-check),
/// otherwise a warning with the group's weakest confidence.
fn group_confidence_text(
    ui: &egui::Ui,
    findings: &[FindingItem],
    indices: &[usize],
) -> egui::RichText {
    let min = group_min_confidence(findings, indices);
    if min < AUTO_SELECT_CONFIDENCE_THRESHOLD {
        egui::RichText::new(format!("\u{26a0} {min}%")).color(ui.visuals().warn_fg_color)
    } else {
        egui::RichText::new("")
    }
}

/// Walks the tree once, skipping the children of any closed
/// disk/game/category/folder, and collects every structurally-visible row.
/// O(number of rows produced): closed subtrees are never descended into, so
/// this stays cheap even when the tree holds tens of thousands of findings.
fn build_visible_rows(tree: &[DiskGroup], toggles: &HashMap<String, bool>) -> Vec<Row> {
    let mut rows = Vec::new();

    for (d, disk_group) in tree.iter().enumerate() {
        rows.push(Row::Disk { d });
        if !is_open(toggles, &disk_key(&disk_group.disk), true) {
            continue;
        }

        for (g, game) in disk_group.games.iter().enumerate() {
            rows.push(Row::Game { d, g });
            if !is_open(toggles, &game_key(&disk_group.disk, game.game_id), false) {
                continue;
            }

            for (c, category_node) in game.categories.iter().enumerate() {
                rows.push(Row::Category { d, g, c });
                let cat_key = category_key(&disk_group.disk, game.game_id, category_node.category);
                if !is_open(toggles, &cat_key, true) {
                    continue;
                }

                for (n, node) in category_node.nodes.iter().enumerate() {
                    match node {
                        TreeNode::Folder {
                            group_dir,
                            item_indices,
                            ..
                        } => {
                            rows.push(Row::Folder { d, g, c, n });
                            let folder_key = folder_key(
                                &disk_group.disk,
                                game.game_id,
                                category_node.category,
                                group_dir,
                            );
                            if !is_open(toggles, &folder_key, false) {
                                continue;
                            }
                            for member in 0..item_indices.len() {
                                rows.push(Row::File {
                                    d,
                                    g,
                                    c,
                                    n,
                                    member: Some(member),
                                });
                            }
                        }
                        TreeNode::File { .. } => {
                            rows.push(Row::File {
                                d,
                                g,
                                c,
                                n,
                                member: None,
                            });
                        }
                    }
                }
            }
        }
    }

    rows
}

/// Stable, collision-free key for a disk row's expand/collapse state.
fn disk_key(disk: &str) -> String {
    format!("d|{disk}")
}

/// Stable, collision-free key for a game row's expand/collapse state.
fn game_key(disk: &str, game_id: i64) -> String {
    format!("g|{disk}|{game_id}")
}

/// Stable, collision-free key for a category row's expand/collapse state.
fn category_key(disk: &str, game_id: i64, category: DisplayCategory) -> String {
    format!("c|{disk}|{game_id}|{}", category_ui_key(category))
}

/// Stable, collision-free key for a folder row's expand/collapse state.
fn folder_key(disk: &str, game_id: i64, category: DisplayCategory, group_dir: &str) -> String {
    format!(
        "f|{disk}|{game_id}|{}|{group_dir}",
        category_ui_key(category)
    )
}

/// Whether a node keyed by `key` is currently open: an explicit user choice
/// if present in `toggles`, otherwise the node kind's default.
fn is_open(toggles: &HashMap<String, bool>, key: &str, default_open: bool) -> bool {
    toggles.get(key).copied().unwrap_or(default_open)
}

/// Nesting level of a row, mirroring the indent used when rendering it.
/// Used by `←` to find a row's structural parent.
fn row_level(row: Row) -> usize {
    match row {
        Row::Disk { .. } => 0,
        Row::Game { .. } => 1,
        Row::Category { .. } => 2,
        Row::Folder { .. } => 3,
        Row::File { member: None, .. } => 3,
        Row::File {
            member: Some(_), ..
        } => 4,
    }
}

/// Index (into `rows`) of the closest preceding row with a lower nesting
/// level - the cursor row's structural parent.
fn parent_row_index(rows: &[Row], index: usize) -> Option<usize> {
    let level = row_level(rows[index]);
    (0..index).rev().find(|&i| row_level(rows[i]) < level)
}

/// The expand/collapse toggle key and default-open state of a row, if the
/// row is expandable at all (file rows are not).
fn row_toggle_key(tree: &[DiskGroup], row: Row) -> Option<(String, bool)> {
    match row {
        Row::Disk { d } => Some((disk_key(&tree[d].disk), true)),
        Row::Game { d, g } => Some((game_key(&tree[d].disk, tree[d].games[g].game_id), false)),
        Row::Category { d, g, c } => {
            let disk_group = &tree[d];
            let game = &disk_group.games[g];
            Some((
                category_key(&disk_group.disk, game.game_id, game.categories[c].category),
                true,
            ))
        }
        Row::Folder { d, g, c, n } => {
            let disk_group = &tree[d];
            let game = &disk_group.games[g];
            let category_node = &game.categories[c];
            let TreeNode::Folder { group_dir, .. } = &category_node.nodes[n] else {
                return None;
            };
            Some((
                folder_key(
                    &disk_group.disk,
                    game.game_id,
                    category_node.category,
                    group_dir,
                ),
                false,
            ))
        }
        Row::File { .. } => None,
    }
}

/// Flat `findings` index of a file row.
fn file_row_index(
    tree: &[DiskGroup],
    d: usize,
    g: usize,
    c: usize,
    n: usize,
    member: Option<usize>,
) -> usize {
    match (&tree[d].games[g].categories[c].nodes[n], member) {
        (TreeNode::Folder { item_indices, .. }, Some(m)) => item_indices[m],
        (TreeNode::File { index }, None) => *index,
        _ => unreachable!("Row::File member/node kind mismatch"),
    }
}

/// Toggles the selection of whatever the row represents: the whole group
/// for header rows, the single file for file rows.
fn toggle_row_selection(tree: &[DiskGroup], findings: &mut [FindingItem], row: Row) {
    match row {
        Row::Disk { d } => toggle_group(findings, &tree[d].all_indices),
        Row::Game { d, g } => toggle_group(findings, &tree[d].games[g].all_indices),
        Row::Category { d, g, c } => {
            toggle_group(findings, &tree[d].games[g].categories[c].all_indices)
        }
        Row::Folder { d, g, c, n } => {
            if let TreeNode::Folder { item_indices, .. } = &tree[d].games[g].categories[c].nodes[n]
            {
                toggle_group(findings, item_indices);
            }
        }
        Row::File { d, g, c, n, member } => {
            let index = file_row_index(tree, d, g, c, n, member);
            findings[index].selected = !findings[index].selected;
        }
    }
}

/// The key presses the tree reacts to in one frame.
struct TreeKeys {
    down: bool,
    up: bool,
    page_down: bool,
    page_up: bool,
    home: bool,
    end: bool,
    toggle_select: bool,
    expand: bool,
    collapse: bool,
}

/// Handles keyboard navigation over the flattened row list. Returns a new
/// scroll offset when the view must jump (PgUp/PgDn scrolling, or keeping
/// the moved cursor visible); `None` leaves the scroll position alone.
fn handle_keyboard(
    app: &mut GameTrimmerApp,
    ui: &egui::Ui,
    rows: &[Row],
    row_stride: f32,
) -> Option<f32> {
    if rows.is_empty() {
        return None;
    }
    // A focused widget (button, checkbox, ...) owns the keyboard - don't
    // fight it over Space/Enter/arrows.
    if ui.ctx().memory(|memory| memory.focused().is_some()) {
        return None;
    }

    let keys = ui.input(|input| TreeKeys {
        down: input.key_pressed(egui::Key::ArrowDown),
        up: input.key_pressed(egui::Key::ArrowUp),
        page_down: input.key_pressed(egui::Key::PageDown),
        page_up: input.key_pressed(egui::Key::PageUp),
        home: input.key_pressed(egui::Key::Home),
        end: input.key_pressed(egui::Key::End),
        toggle_select: input.key_pressed(egui::Key::Space) || input.key_pressed(egui::Key::Enter),
        expand: input.key_pressed(egui::Key::ArrowRight),
        collapse: input.key_pressed(egui::Key::ArrowLeft),
    });

    let last = rows.len() - 1;
    let page = ((app.tree_viewport_height / row_stride).floor() as usize).max(1);

    // Without an active cursor, the paging/jump keys scroll the list as-is.
    if app.tree_cursor.is_none() {
        if keys.page_down {
            return Some(app.tree_scroll_offset + app.tree_viewport_height);
        }
        if keys.page_up {
            return Some((app.tree_scroll_offset - app.tree_viewport_height).max(0.0));
        }
        if keys.home {
            return Some(0.0);
        }
        if keys.end {
            return Some(rows.len() as f32 * row_stride);
        }
    }

    let mut cursor = app.tree_cursor;
    let mut moved = false;

    if keys.down {
        cursor = Some(cursor.map_or(0, |current| (current + 1).min(last)));
        moved = true;
    }
    if keys.up {
        cursor = Some(cursor.map_or(last, |current| current.saturating_sub(1)));
        moved = true;
    }
    if keys.page_down {
        if let Some(current) = cursor {
            cursor = Some((current + page).min(last));
            moved = true;
        }
    }
    if keys.page_up {
        if let Some(current) = cursor {
            cursor = Some(current.saturating_sub(page));
            moved = true;
        }
    }
    if keys.home && cursor.is_some() {
        cursor = Some(0);
        moved = true;
    }
    if keys.end && cursor.is_some() {
        cursor = Some(last);
        moved = true;
    }

    if let Some(current) = cursor {
        let row = rows[current.min(last)];
        if keys.toggle_select {
            toggle_row_selection(&app.tree, &mut app.findings, row);
        }
        if keys.expand || keys.collapse {
            match row_toggle_key(&app.tree, row) {
                Some((key, default_open)) => {
                    let open = is_open(&app.tree_toggles, &key, default_open);
                    if keys.expand && !open {
                        app.tree_toggles.insert(key, true);
                    } else if keys.collapse && open {
                        app.tree_toggles.insert(key, false);
                    } else if keys.collapse {
                        // Already collapsed: jump to the parent instead.
                        if let Some(parent) = parent_row_index(rows, current) {
                            cursor = Some(parent);
                            moved = true;
                        }
                    }
                }
                None => {
                    if keys.collapse {
                        if let Some(parent) = parent_row_index(rows, current) {
                            cursor = Some(parent);
                            moved = true;
                        }
                    }
                }
            }
        }
    }

    app.tree_cursor = cursor;

    if moved {
        let current = cursor?;
        let top = current as f32 * row_stride;
        let bottom = top + row_stride;
        let view_height = app.tree_viewport_height.max(row_stride);
        if top < app.tree_scroll_offset {
            return Some(top);
        }
        if bottom > app.tree_scroll_offset + view_height {
            return Some(bottom - view_height);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn show_row(
    ui: &mut egui::Ui,
    tree: &[DiskGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    row: Row,
    row_index: usize,
    lang: Lang,
) {
    // Highlight the keyboard-cursor row behind its widgets.
    if *cursor == Some(row_index) {
        let rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
        );
        ui.painter().rect_filled(
            rect,
            2.0,
            ui.visuals().selection.bg_fill.gamma_multiply(0.35),
        );
    }

    match row {
        Row::Disk { d } => show_disk_row(ui, tree, findings, toggles, cursor, d, row_index, lang),
        Row::Game { d, g } => {
            show_game_row(ui, tree, findings, toggles, cursor, d, g, row_index, lang)
        }
        Row::Category { d, g, c } => show_category_row(
            ui, tree, findings, toggles, cursor, d, g, c, row_index, lang,
        ),
        Row::Folder { d, g, c, n } => show_folder_row(
            ui, tree, findings, toggles, cursor, d, g, c, n, row_index, lang,
        ),
        Row::File { d, g, c, n, member } => show_file_row(
            ui, tree, findings, cursor, d, g, c, n, member, row_index, lang,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn show_disk_row(
    ui: &mut egui::Ui,
    tree: &[DiskGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    row_index: usize,
    lang: Lang,
) {
    let disk_group = &tree[d];
    let key = disk_key(&disk_group.disk);
    let name = egui::RichText::new(i18n::disk_label(lang, &disk_group.disk)).strong();
    let response = show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        true,
        0,
        &disk_group.all_indices,
        disk_group.total_bytes,
        name,
        lang,
    );
    response.context_menu(|ui| {
        if ui
            .button(i18n::select_all_on_disk(lang, &disk_group.disk))
            .clicked()
        {
            set_group_selection(findings, &disk_group.all_indices, true);
            ui.close();
        }
        if ui
            .button(i18n::deselect_all_on_disk(lang, &disk_group.disk))
            .clicked()
        {
            set_group_selection(findings, &disk_group.all_indices, false);
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn show_game_row(
    ui: &mut egui::Ui,
    tree: &[DiskGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    row_index: usize,
    lang: Lang,
) {
    let disk_group = &tree[d];
    let game = &disk_group.games[g];
    let key = game_key(&disk_group.disk, game.game_id);
    let name = egui::RichText::new(i18n::quoted(lang, &game.game_name)).strong();
    let response = show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        false,
        1,
        &game.all_indices,
        game.total_bytes,
        name,
        lang,
    );
    response.context_menu(|ui| {
        if ui
            .button(i18n::select_all_in_game(lang, &game.game_name))
            .clicked()
        {
            set_group_selection(findings, &game.all_indices, true);
            ui.close();
        }
        if ui
            .button(i18n::deselect_all_in_game(lang, &game.game_name))
            .clicked()
        {
            set_group_selection(findings, &game.all_indices, false);
            ui.close();
        }
    });
}

/// Every finding of `category` across all games of one disk - the target of
/// the "select category across the whole disk" bulk action.
fn category_indices_on_disk(disk_group: &DiskGroup, category: DisplayCategory) -> Vec<usize> {
    disk_group
        .games
        .iter()
        .flat_map(|game| {
            game.categories
                .iter()
                .filter(|category_node| category_node.category == category)
                .flat_map(|category_node| category_node.all_indices.iter().copied())
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn show_category_row(
    ui: &mut egui::Ui,
    tree: &[DiskGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    c: usize,
    row_index: usize,
    lang: Lang,
) {
    let disk_group = &tree[d];
    let game = &disk_group.games[g];
    let category_node = &game.categories[c];
    let key = category_key(&disk_group.disk, game.game_id, category_node.category);
    let name = egui::RichText::new(category_display(lang, category_node.category));
    let response = show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        true,
        2,
        &category_node.all_indices,
        category_node.total_bytes,
        name,
        lang,
    );
    response.context_menu(|ui| {
        let label = category_display(lang, category_node.category);
        if ui
            .button(i18n::select_category_on_disk(lang, label, &disk_group.disk))
            .clicked()
        {
            let indices = category_indices_on_disk(disk_group, category_node.category);
            set_group_selection(findings, &indices, true);
            ui.close();
        }
        if ui
            .button(i18n::deselect_category_on_disk(
                lang,
                label,
                &disk_group.disk,
            ))
            .clicked()
        {
            let indices = category_indices_on_disk(disk_group, category_node.category);
            set_group_selection(findings, &indices, false);
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn show_folder_row(
    ui: &mut egui::Ui,
    tree: &[DiskGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    c: usize,
    n: usize,
    row_index: usize,
    lang: Lang,
) {
    let disk_group = &tree[d];
    let game = &disk_group.games[g];
    let category_node = &game.categories[c];
    let TreeNode::Folder {
        group_dir,
        item_indices,
        total_bytes,
    } = &category_node.nodes[n]
    else {
        unreachable!("Row::Folder always points at a TreeNode::Folder");
    };
    let key = folder_key(
        &disk_group.disk,
        game.game_id,
        category_node.category,
        group_dir,
    );
    let name = egui::RichText::new(format!("{group_dir}\\"));
    show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        false,
        3,
        item_indices,
        *total_bytes,
        name,
        lang,
    );
}

/// Renders one expandable header row shared by disk/game/category/folder
/// rows: an expand/collapse arrow button, a tri-state checkbox over
/// `indices`, the name, and the fixed columns (count, size, and a
/// weakest-confidence warning when the group needs a closer look).
/// Returns the name label's response so callers can attach a context menu.
#[allow(clippy::too_many_arguments)]
fn show_header_row(
    ui: &mut egui::Ui,
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    row_index: usize,
    key: &str,
    default_open: bool,
    level: usize,
    indices: &[usize],
    total_bytes: u64,
    name: egui::RichText,
    lang: Lang,
) -> egui::Response {
    let confidence = group_confidence_text(ui, findings, indices);
    let mut name_response = None;

    row_columns(
        ui,
        egui::RichText::new(""),
        egui::RichText::new(indices.len().to_string()),
        egui::RichText::new(format_size(lang, total_bytes)),
        confidence,
        |ui| {
            ui.add_space(INDENT_PX * level as f32);

            let open = is_open(toggles, key, default_open);
            if ui
                .button(if open { "\u{25bc}" } else { "\u{25b6}" })
                .clicked()
            {
                toggles.insert(key.to_string(), !open);
            }

            let (all_selected, any_selected) = group_selection_state(findings, indices);
            let mut checked = all_selected;
            let response = ui.add(
                egui::Checkbox::new(&mut checked, "").indeterminate(any_selected && !all_selected),
            );
            if response.clicked() {
                toggle_group(findings, indices);
            }

            let response = ui.add(
                egui::Label::new(name)
                    .truncate()
                    .sense(egui::Sense::click()),
            );
            if response.clicked() {
                *cursor = Some(row_index);
            }
            name_response = Some(response);
        },
    );

    name_response.expect("row_columns always runs the left closure")
}

/// Renders one file row: checkbox, name, and the language/size/confidence
/// columns. The classification reason lives in the name's hover tooltip
/// rather than inline - being under a category header already explains the
/// row, and the extra text would just be noise (details on demand).
#[allow(clippy::too_many_arguments)]
fn show_file_row(
    ui: &mut egui::Ui,
    tree: &[DiskGroup],
    findings: &mut [FindingItem],
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    c: usize,
    n: usize,
    member: Option<usize>,
    row_index: usize,
    lang: Lang,
) {
    let node = &tree[d].games[g].categories[c].nodes[n];
    let (index, level, display_name) = match (node, member) {
        (
            TreeNode::Folder {
                group_dir,
                item_indices,
                ..
            },
            Some(m),
        ) => {
            let index = item_indices[m];
            let rel_path = &findings[index].row.rel_path;
            // Members render under their folder header - repeating the
            // folder prefix on every line would defeat the grouping.
            let name = rel_path
                .strip_prefix(&format!("{group_dir}\\"))
                .unwrap_or(rel_path)
                .to_string();
            (index, 4, name)
        }
        (TreeNode::File { index }, None) => (*index, 3, findings[*index].row.rel_path.clone()),
        _ => unreachable!("Row::File member/node kind mismatch"),
    };

    let item = &mut findings[index];

    let lang_col = match &item.row.lang_tag {
        Some(lang_tag) => egui::RichText::new(format!("[{lang_tag}]")),
        None => egui::RichText::new(""),
    };
    let confidence = if item.row.confidence < AUTO_SELECT_CONFIDENCE_THRESHOLD {
        egui::RichText::new(format!("\u{26a0} {}%", item.row.confidence))
            .color(ui.visuals().warn_fg_color)
    } else {
        egui::RichText::new(format!("{}%", item.row.confidence))
    };

    let mut hover = i18n::hover_reason(
        lang,
        &item.row.rel_path,
        &item.row.rule_desc,
        item.row.confidence,
    );
    if let Some(lang_tag) = &item.row.lang_tag {
        hover.push_str(&i18n::hover_lang_suffix(lang, lang_tag));
    }

    row_columns(
        ui,
        lang_col,
        egui::RichText::new(""),
        egui::RichText::new(format_size(lang, item.row.size)),
        confidence,
        |ui| {
            ui.add_space(INDENT_PX * level as f32);
            ui.checkbox(&mut item.selected, "");
            let response = ui
                .add(
                    egui::Label::new(display_name)
                        .truncate()
                        .sense(egui::Sense::click()),
                )
                .on_hover_text(hover);
            if response.clicked() {
                *cursor = Some(row_index);
            }
        },
    );
}
