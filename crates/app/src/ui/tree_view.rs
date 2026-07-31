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
//! The cursor row is highlighted; clicking anywhere on a row places the
//! cursor there (GT-32).
//!
//! # Why the row is one click target
//!
//! Placing the cursor used to require hitting the name label itself - a few
//! dozen pixels on a row that spans the whole panel, with the rest of the
//! width inert. Every row therefore registers a full-width background
//! [`egui::Ui::interact`] rect *before* its own widgets, so the checkbox, the
//! expand arrow and the name still win the click where they are (egui
//! hit-tests later-registered widgets as being on top) while everything
//! between and around them lands on the row.
//!
//! Note what a row click deliberately does *not* do: it moves the cursor, it
//! never ticks the row's checkbox. In a tool that deletes files those are two
//! different things, and a stray click on a wide target must not be able to
//! mark anything for removal - marking stays on the checkbox and on the
//! explicit `Space`/`Enter` toggle.

use std::collections::HashMap;
use std::path::Path;

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n::{self, Lang};
use crate::model::{
    self, category_display, category_ui_key, format_size, group_selection_state, is_orphan_branch,
    set_group_selection, toggle_group, DiskGroup, DisplayCategory, FindingItem, GameNode, TreeNode,
    AUTO_SELECT_CONFIDENCE_THRESHOLD,
};
use crate::search::SearchIndex;
use crate::ui::row_actions;

/// Horizontal indent per nesting level (disk = 0, game = 1, category = 2,
/// folder = 3, orphan file = 3, folder member = 4).
const INDENT_PX: f32 = 18.0;
/// Fixed column widths (right-aligned block), so every row lines up into
/// the same columns as the header row above the list.
const LANG_COLUMN_PX: f32 = 48.0;
const COUNT_COLUMN_PX: f32 = 64.0;
const SIZE_COLUMN_PX: f32 = 92.0;

/// Width held before every file name for the "look at this one" mark, so the
/// names of marked and unmarked rows still line up in one column.
///
/// There used to be a fifth right-hand column here, "Confidence", carrying a
/// percentage per row and the weakest percentage per group. It was the
/// detector's own scale leaking onto the screen: a user cannot weigh a
/// decision against "72%", and the one decision the number actually drove -
/// whether the row was ticked for them - is a yes/no. So the number is gone
/// from the tree (it survives in the row's tooltip, beside the rule that
/// produced it, and in the CSV export) and what is left is the mark.
const REVIEW_MARK_PX: f32 = 18.0;

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

    // Every selection edit reachable from this panel - per-file checkbox,
    // tri-state group checkboxes, keyboard toggle, context-menu actions -
    // happens through a disjoint `&mut app.findings` borrow with no way to
    // call back into `app`. Rather than hooking each site and hoping the list
    // stays complete, take a fingerprint around the whole pass and compare.
    // Worker messages that also touch `findings` (a fresh scan applying the
    // profile, files disappearing mid-delete) are handled in
    // `drain_messages`, outside this window, so they cannot be mistaken for a
    // hand-edit.
    let selection_before = model::selection_fingerprint(&app.findings);

    egui::CentralPanel::default().show(ui, |ui| {
        // Before the first scan - and before the disclaimer is accepted -
        // this space carries the introduction (GT-34) rather than one line of
        // hint text. See `ui::onboarding` for why it lives here and not in a
        // screen of its own.
        //
        // Ahead of the plan cards and of the empty-tree branch alike, because
        // it now also covers the upgrade case, where findings from a previous
        // version are already loaded: cards summarising a tree the user
        // cannot act on yet would be an invitation to a refusal.
        if crate::ui::onboarding::applies(app) {
            crate::ui::onboarding::show(app, ui);
            return;
        }

        // GT-03 plan cards ride at the top of this panel, directly above the
        // tree, so the summary and the drill-down live in one always-visible
        // region (a no-op with no findings). The tree's own scroll area below
        // takes whatever height remains.
        crate::ui::plan_panel::show(app, ui);

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
        // own the keyboard while open - see `GameTrimmerApp::any_modal_open`,
        // which is the single list of them (this used to enumerate three of
        // the five here and silently kept handling keys behind the settings
        // dialog and the clear-database confirmation).
        let mut scroll_override = None;
        if !app.any_modal_open() {
            let rows = build_visible_rows(
                &app.tree,
                &app.tree_toggles,
                app.tree_category_filter,
                &app.tree_search_index,
            );
            scroll_override = handle_keyboard(app, ui, &rows, row_stride);
        }

        // Rebuilt after key handling: expanding/collapsing above changes
        // which rows are visible.
        let rows = build_visible_rows(
            &app.tree,
            &app.tree_toggles,
            app.tree_category_filter,
            &app.tree_search_index,
        );
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

        // Keep the scrollbar drawn even when the content happens to fit, so the
        // list's right edge (and the width available to file rows) stays fixed
        // instead of jumping as items are filtered/toggled in and out of view.
        let mut scroll_area = egui::ScrollArea::vertical()
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible);
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

    // The profile picker claims to describe what is checked. Once the user
    // has hand-edited any of it, only "Custom" is still true (audit §5.5).
    if model::selection_fingerprint(&app.findings) != selection_before {
        app.mark_selection_custom();
    }
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
        |ui| {
            ui.add_space(4.0);
            ui.strong(s.col_name);
        },
    );
}

/// Lays out one row as [flexible left part | Language | Files | Size], with
/// the three fixed-width columns right-aligned against the panel edge. Every
/// row (headers and files alike) goes through this, which is what keeps the
/// table aligned without a real grid widget.
fn row_columns(
    ui: &mut egui::Ui,
    lang: egui::RichText,
    count: egui::RichText,
    size: egui::RichText,
    left: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

/// The leading cell that carries a file row's "look at this one" mark.
///
/// Always allocated, marked or not, so the file names below a folder header
/// stay in one column instead of stepping sideways row by row.
///
/// The mark is the whole of what the removed confidence column used to say
/// that a user could act on: this file was left unticked on purpose. The
/// tooltip spells that out - a bare \u{26a0} with no explanation is how the
/// percentage got its reputation in the first place.
fn show_review_mark(ui: &mut egui::Ui, needs_review: bool, hint: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(REVIEW_MARK_PX, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(REVIEW_MARK_PX);
            if needs_review {
                ui.label(egui::RichText::new("\u{26a0}").color(ui.visuals().warn_fg_color))
                    .on_hover_text(hint);
            }
        },
    );
}

/// Walks the tree once, skipping the children of any closed
/// disk/game/category/folder, and collects every structurally-visible row.
/// O(number of rows produced): closed subtrees are never descended into, so
/// this stays cheap even when the tree holds tens of thousands of findings.
/// Whether a game has any node under the active plan-card filter (GT-03).
/// `None` filter matches everything.
fn game_matches_filter(game: &crate::model::GameNode, filter: Option<DisplayCategory>) -> bool {
    match filter {
        None => true,
        Some(category) => game.categories.iter().any(|node| node.category == category),
    }
}

/// Whether a disk has any game with a node under the active filter (GT-03).
fn disk_matches_filter(disk_group: &DiskGroup, filter: Option<DisplayCategory>) -> bool {
    disk_group
        .games
        .iter()
        .any(|game| game_matches_filter(game, filter))
}

/// Whether a game still has content under the active name search (GT-18).
///
/// Real games answer this in O(1) from the pre-built id set. The orphan branch
/// cannot: every disk's branch shares the one [`is_orphan_branch`] sentinel id,
/// so the id-keyed answer would light up every disk's branch as soon as any one
/// of them matched. Its findings are the launcher-residue leftovers - few
/// enough to scan directly.
fn game_matches_search(game: &GameNode, search: &SearchIndex) -> bool {
    if !search.is_active() {
        return true;
    }
    if is_orphan_branch(game.game_id) {
        search.any_matches(&game.all_indices)
    } else {
        search.game_matches(game.game_id)
    }
}

fn build_visible_rows(
    tree: &[DiskGroup],
    toggles: &HashMap<String, bool>,
    filter: Option<DisplayCategory>,
    search: &SearchIndex,
) -> Vec<Row> {
    let mut rows = Vec::new();

    for (d, disk_group) in tree.iter().enumerate() {
        // Under a plan-card filter or a name search, a disk (and each game)
        // with nothing left to show is skipped entirely rather than shown as an
        // empty header, so the "View" action lands on exactly that category's
        // findings and a search shows only branches that contain a hit.
        if !disk_matches_filter(disk_group, filter) {
            continue;
        }
        if !disk_group
            .games
            .iter()
            .any(|game| game_matches_search(game, search))
        {
            continue;
        }
        rows.push(Row::Disk { d });
        if !is_open(toggles, &disk_key(&disk_group.disk), true) {
            continue;
        }

        for (g, game) in disk_group.games.iter().enumerate() {
            if !game_matches_filter(game, filter) || !game_matches_search(game, search) {
                continue;
            }
            rows.push(Row::Game { d, g });
            if !is_open(toggles, &game_key(&disk_group.disk, game.game_id), false) {
                continue;
            }

            for (c, category_node) in game.categories.iter().enumerate() {
                if filter.is_some_and(|category| category_node.category != category) {
                    continue;
                }
                // Only reached for an expanded game, so the per-item scans from
                // here down are bounded by what the user has opened - never by
                // the size of the whole result set.
                if search.is_active() && !search.any_matches(&category_node.all_indices) {
                    continue;
                }
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
                            if search.is_active() && !search.any_matches(item_indices) {
                                continue;
                            }
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
                            for (member, &index) in item_indices.iter().enumerate() {
                                // A folder whose own name matched has every
                                // member matched too (the query is a substring
                                // of each member's path), so this only filters
                                // when the hit was on individual files.
                                if search.is_active() && !search.item_matches(index) {
                                    continue;
                                }
                                rows.push(Row::File {
                                    d,
                                    g,
                                    c,
                                    n,
                                    member: Some(member),
                                });
                            }
                        }
                        TreeNode::File { index } => {
                            if search.is_active() && !search.item_matches(*index) {
                                continue;
                            }
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
    let row_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
    );

    // GT-32: the whole row is a click target. Registered here, before the
    // row's own widgets, so those stay on top and keep their own clicks -
    // see this module's "Why the row is one click target".
    let background = ui.interact(
        row_rect,
        ui.id().with(("tree_row", row_index)),
        egui::Sense::click(),
    );

    // Highlight the keyboard-cursor row behind its widgets, and give the
    // row a hover tint so the widened target is visible before it is used.
    // `contains_pointer` rather than `hovered`: the pointer sitting on the
    // row's own checkbox or name makes those the hovered widget, and the
    // row underneath must not blink out from under them.
    if *cursor == Some(row_index) {
        ui.painter().rect_filled(
            row_rect,
            2.0,
            ui.visuals().selection.bg_fill.gamma_multiply(0.35),
        );
    } else if background.contains_pointer() {
        ui.painter()
            .rect_filled(row_rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }

    if background.clicked() {
        *cursor = Some(row_index);
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
    let target = ShellTarget::Folder(disk_root_path(&disk_group.disk));
    let response = response.on_hover_text(target.path().to_string());
    row_context_menu(&response, lang, Some(target), |ui| {
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
    // The orphan branch (GT-02) has no real game name - render a localized
    // "orphaned residue" label instead of a quoted game title. Computed live
    // off the sentinel id, so it follows the current UI language even though
    // the stored `game_name` is empty.
    let label = game_branch_label(lang, game);
    let name = if is_orphan_branch(game.game_id) {
        egui::RichText::new(label.clone()).strong()
    } else {
        egui::RichText::new(i18n::quoted(lang, &label)).strong()
    };
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
    // The game's install dir, taken from any of its findings (they all share
    // it). Absent on the orphan branch (GT-02): its findings are residue from
    // different games, so there is no one folder the row could stand for.
    let target = if is_orphan_branch(game.game_id) {
        None
    } else {
        install_dir_of(findings, &game.all_indices).map(ShellTarget::Folder)
    };
    let response = match &target {
        Some(target) => response.on_hover_text(target.path().to_string()),
        None => response,
    };

    row_context_menu(&response, lang, target, |ui| {
        if ui.button(i18n::select_all_in_game(lang, &label)).clicked() {
            set_group_selection(findings, &game.all_indices, true);
            ui.close();
        }
        if ui
            .button(i18n::deselect_all_in_game(lang, &label))
            .clicked()
        {
            set_group_selection(findings, &game.all_indices, false);
            ui.close();
        }
    });
}

/// The label shown for a game node: the orphan branch's localized
/// "orphaned residue" heading (GT-02) when this is the synthetic orphan
/// pseudo-game, otherwise the real game's own name.
fn game_branch_label(lang: Lang, game: &GameNode) -> String {
    if is_orphan_branch(game.game_id) {
        i18n::strings(lang).orphan_branch_label.to_string()
    } else {
        game.game_name.clone()
    }
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
    // A category is a slice of one game, so it stands for that game's install
    // dir - the same folder its parent row opens.
    let target = if is_orphan_branch(game.game_id) {
        None
    } else {
        install_dir_of(findings, &category_node.all_indices).map(ShellTarget::Folder)
    };
    let response = match &target {
        Some(target) => response.on_hover_text(target.path().to_string()),
        None => response,
    };

    row_context_menu(&response, lang, target, |ui| {
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
    let response = show_header_row(
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

    // The folder's absolute path comes from any member (they all share the
    // game's install dir); the same path drives the hover tooltip and the
    // context-menu actions.
    if let Some(&first) = item_indices.first() {
        let abs_path =
            row_actions::windows_path_string(&findings[first].row.install_dir.join(group_dir));
        let response = response.on_hover_text(abs_path.clone());
        row_context_menu(
            &response,
            lang,
            Some(ShellTarget::Folder(abs_path)),
            |_ui| {},
        );
    }
}

/// Renders one expandable header row shared by disk/game/category/folder
/// rows: an expand/collapse arrow button, a tri-state checkbox over
/// `indices`, the name, and the fixed columns (count and size).
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
    let mut name_response = None;

    row_columns(
        ui,
        egui::RichText::new(""),
        egui::RichText::new(indices.len().to_string()),
        egui::RichText::new(format_size(lang, total_bytes)),
        |ui| {
            ui.add_space(INDENT_PX * level as f32);

            // Selection checkbox first, then the expand/collapse arrow: the
            // checkbox is the row's primary control and belongs in the same
            // leading column as the file-row checkboxes (which have no arrow),
            // so the selection column reads consistently down the tree.
            let (all_selected, any_selected) = group_selection_state(findings, indices);
            let mut checked = all_selected;
            let response = ui.add(
                egui::Checkbox::new(&mut checked, "").indeterminate(any_selected && !all_selected),
            );
            if response.clicked() {
                toggle_group(findings, indices);
            }

            let open = is_open(toggles, key, default_open);
            if ui
                .button(if open { "\u{25bc}" } else { "\u{25b6}" })
                .clicked()
            {
                toggles.insert(key.to_string(), !open);
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

/// What a row's filesystem actions operate on. Every row in the tree stands
/// for something on disk - a drive, a game's install dir, a folder, a file -
/// so every row offers these actions; only the shape of "open it" differs.
enum ShellTarget {
    /// A directory (disk root, game install dir, folder): Explorer opens it.
    Folder(String),
    /// A file: Explorer opens the containing folder with the file highlighted
    /// (there is nothing to "open" about a file in Explorer otherwise), and
    /// the "Open with..." chooser applies - it is a file-only system dialog.
    File(String),
}

impl ShellTarget {
    /// The absolute path, in Windows-native form, either variant points at.
    fn path(&self) -> &str {
        match self {
            Self::Folder(path) | Self::File(path) => path,
        }
    }
}

/// Emits the filesystem actions shared by every row: open in Explorer,
/// "Open with..." for files, and copy the path. Called at the top of each
/// row's context menu, above that row's own selection actions.
///
/// Launch failures are logged (the app's convention for non-fatal errors),
/// never surfaced mid-menu.
fn shell_actions(ui: &mut egui::Ui, lang: Lang, target: &ShellTarget) {
    let s = i18n::strings(lang);
    let path = Path::new(target.path());

    if ui.button(s.ctx_reveal_in_explorer).clicked() {
        let (program, args) = match target {
            ShellTarget::Folder(_) => row_actions::open_folder_args(path),
            ShellTarget::File(_) => row_actions::reveal_in_explorer_args(path),
        };
        if let Err(err) = row_actions::launch(program, &args) {
            crate::logger::log(&format!("Не вдалося відкрити Провідник: {err}"));
        }
        ui.close();
    }

    if matches!(target, ShellTarget::File(_)) && ui.button(s.ctx_open_with).clicked() {
        let (program, args) = row_actions::open_with_args(path);
        if let Err(err) = row_actions::launch(program, &args) {
            crate::logger::log(&format!(
                "Не вдалося відкрити діалог «Відкрити за допомогою»: {err}"
            ));
        }
        ui.close();
    }

    if ui.button(s.ctx_copy_path).clicked() {
        ui.ctx().copy_text(target.path().to_string());
        ui.close();
    }
}

/// Attaches a row's right-click context menu: the shared filesystem actions
/// for `target`, then - separated - whatever selection actions that row type
/// adds via `own_actions`. A row with no path behind it (only the orphan
/// branch, whose findings come from different games) passes `None` and shows
/// just its own actions.
fn row_context_menu(
    response: &egui::Response,
    lang: Lang,
    target: Option<ShellTarget>,
    own_actions: impl FnOnce(&mut egui::Ui),
) {
    response.context_menu(|ui| {
        if let Some(target) = &target {
            shell_actions(ui, lang, target);
            ui.separator();
        }
        own_actions(ui);
    });
}

/// The filesystem root a disk group stands for: a drive letter row (`F:`)
/// means `F:\`, while a UNC group (`\\server\share`) already is a path.
fn disk_root_path(disk: &str) -> String {
    if disk.ends_with(':') {
        format!("{disk}\\")
    } else {
        disk.to_string()
    }
}

/// The install directory shared by `indices` (every finding of one game lives
/// under the same one), in Windows-native form. `None` when the group is empty.
fn install_dir_of(findings: &[FindingItem], indices: &[usize]) -> Option<String> {
    let &first = indices.first()?;
    Some(row_actions::windows_path_string(
        &findings[first].row.install_dir,
    ))
}

/// Renders one file row: checkbox, review mark, name, and the language/size
/// columns. The classification reason lives in the name's hover tooltip
/// rather than inline - being under a category header already explains the
/// row, and the extra text would just be noise (details on demand). The raw
/// confidence figure went the same way, for the same reason.
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
    // The same threshold that decided whether this row was ticked for the
    // user after the scan - so the mark and the checkbox never disagree.
    let needs_review = item.row.confidence < AUTO_SELECT_CONFIDENCE_THRESHOLD;
    let review_hint = i18n::strings(lang).review_mark_hint;

    // Absolute path (Windows-native separators): the tooltip's first line, the
    // clipboard payload, and the argument to every context-menu shell action.
    let abs_path = row_actions::windows_path_string(&item.row.install_dir.join(&item.row.rel_path));

    let mut hover = i18n::hover_reason(lang, &abs_path, &item.row.rule_desc, item.row.confidence);
    if let Some(lang_tag) = &item.row.lang_tag {
        hover.push_str(&i18n::hover_lang_suffix(lang, lang_tag));
    }
    // The row shows the on-disk allocated size as primary (GT-05a); when the
    // logical size differs (cluster slack, NTFS compression), spell it out in
    // the tooltip so the two figures are both available without cluttering the
    // row.
    if item.row.size != item.row.size_on_disk {
        hover.push_str(&i18n::hover_logical_size_suffix(
            lang,
            &format_size(lang, item.row.size),
        ));
    }

    row_columns(
        ui,
        lang_col,
        egui::RichText::new(""),
        egui::RichText::new(format_size(lang, item.row.size_on_disk)),
        |ui| {
            ui.add_space(INDENT_PX * level as f32);
            ui.checkbox(&mut item.selected, "");
            show_review_mark(ui, needs_review, review_hint);
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
            row_context_menu(
                &response,
                lang,
                Some(ShellTarget::File(abs_path.clone())),
                |_ui| {},
            );
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use gametrimmer_core::settings::SelectionProfile;

    use crate::ui::harness::UiTest;

    /// The name of the first seeded finding's row, as the tree draws it.
    const SEEDED_FILE_NAME: &str = "data/loc_0.pak";

    /// Seeded findings with every branch opened and one confidence per
    /// finding, in seed order.
    ///
    /// The opening is not optional: games and categories are collapsed by
    /// default, so a test that only calls `seed_findings` sees three header
    /// rows and no file rows at all - and every claim below is about a file
    /// row.
    fn tree_of_files(confidences: [u8; 2]) -> UiTest {
        let mut test = UiTest::new(show);
        test.seed_findings();
        assert_eq!(
            test.app().findings.len(),
            confidences.len(),
            "the seed no longer produces one finding per confidence given here",
        );
        for (item, confidence) in test.app_mut().findings.iter_mut().zip(confidences) {
            item.row.confidence = confidence;
        }

        let mut keys = Vec::new();
        for disk_group in &test.app().tree {
            keys.push(disk_key(&disk_group.disk));
            for game in &disk_group.games {
                keys.push(game_key(&disk_group.disk, game.game_id));
                for category_node in &game.categories {
                    keys.push(category_key(
                        &disk_group.disk,
                        game.game_id,
                        category_node.category,
                    ));
                }
            }
        }
        for key in keys {
            test.app_mut().tree_toggles.insert(key, true);
        }
        test.run();

        test.assert_label(SEEDED_FILE_NAME);
        test
    }

    /// The confidence percentage is gone from the tree - header and rows
    /// alike. It was the detector's internal scale, unactionable on its own,
    /// and it cost a fixed 72pt column on every row of the densest screen in
    /// the app.
    ///
    /// Both sides of the threshold, because the old column printed a figure
    /// either way - plain above it, warning-coloured below.
    #[test]
    fn no_row_shows_a_confidence_percentage() {
        for confidence in [
            AUTO_SELECT_CONFIDENCE_THRESHOLD - 1,
            AUTO_SELECT_CONFIDENCE_THRESHOLD,
        ] {
            let test = tree_of_files([confidence; 2]);

            assert_eq!(
                test.count_labels_containing(&format!("{confidence}%")),
                0,
                "a row still reports its confidence as {confidence}%",
            );
            // The header lost the column too. Literals rather than a
            // `strings()` lookup on purpose: the field they used to name is
            // gone, which is exactly the claim.
            for heading in ["Confidence", "\u{414}\u{43e}\u{432}\u{456}\u{440}\u{430}"] {
                test.assert_no_label(heading);
            }
        }
    }

    /// What replaced it. A finding below the auto-select threshold is the one
    /// the scan deliberately left unticked, so the row has to say so - and
    /// say why, rather than leaving a bare \u{26a0} to be guessed at.
    ///
    /// One marked finding beside one confident one, so the hover lands on a
    /// single mark and the mark is visibly per row rather than per tree.
    #[test]
    fn a_finding_left_unticked_carries_a_mark_that_explains_itself() {
        let mut test = tree_of_files([
            AUTO_SELECT_CONFIDENCE_THRESHOLD - 1,
            AUTO_SELECT_CONFIDENCE_THRESHOLD,
        ]);
        assert_eq!(test.count_labels("\u{26a0}"), 1, "one row is below the bar");

        test.hover("\u{26a0}");

        test.assert_label(test.strings().review_mark_hint);
    }

    /// The control: a confident finding gets no mark. Without this the
    /// assertion above would pass just as well if every row were marked,
    /// which would make the mark furniture.
    #[test]
    fn a_confident_finding_carries_no_mark() {
        let test = tree_of_files([AUTO_SELECT_CONFIDENCE_THRESHOLD; 2]);

        assert_eq!(
            test.count_labels("\u{26a0}"),
            0,
            "a finding at the auto-select threshold was still marked for review",
        );
    }

    /// The mark sits in a cell of its own width, held whether or not it is
    /// drawn, so a marked row does not push its name sideways out of the
    /// column its neighbours line up in.
    #[test]
    fn the_mark_does_not_shift_the_name_column() {
        let marked = tree_of_files([AUTO_SELECT_CONFIDENCE_THRESHOLD - 1; 2])
            .rect_of(SEEDED_FILE_NAME)
            .min
            .x;
        let unmarked = tree_of_files([AUTO_SELECT_CONFIDENCE_THRESHOLD; 2])
            .rect_of(SEEDED_FILE_NAME)
            .min
            .x;

        assert_eq!(
            marked, unmarked,
            "the review mark moved the file name: {marked} vs {unmarked}",
        );
    }

    /// A tree with a cursor parked on the first row, ready for an arrow key.
    fn tree_with_cursor() -> UiTest {
        let mut test = UiTest::new(show);
        test.seed_findings();
        test.app_mut().tree_cursor = Some(0);
        test.run();
        test
    }

    /// The control: with nothing modal open, the tree owns the keyboard.
    /// Without this, the assertions below would also pass if arrow keys had
    /// simply stopped working altogether.
    #[test]
    fn arrow_keys_move_the_cursor_when_no_dialog_is_open() {
        let mut test = tree_with_cursor();

        test.press(egui::Key::ArrowDown);

        assert_eq!(
            test.app().tree_cursor,
            Some(1),
            "the tree should handle arrow keys when nothing is modal",
        );
    }

    /// The bug (audit §6.12): the tree's own modal list named three of the
    /// five dialogs, so keys still reached it behind Settings and behind the
    /// clear-database confirmation. Every modal is checked here, not just the
    /// two that were missing - the point of routing through
    /// `any_modal_open` is that this list cannot drift again.
    /// One modal, named for the failure message, and how to open it.
    type OpenModal = (&'static str, fn(&mut GameTrimmerApp));

    #[test]
    fn no_modal_lets_arrow_keys_reach_the_tree() {
        let modals: [OpenModal; 5] = [
            ("show_settings", |app| app.show_settings = true),
            ("confirm_clear_database", |app| {
                app.confirm_clear_database = true
            }),
            ("show_elevation_prompt", |app| {
                app.show_elevation_prompt = true
            }),
            ("confirm_delete", |app| {
                app.confirm_delete = Some(crate::app::ConfirmDelete {
                    indices: vec![0],
                    method: gametrimmer_core::settings::DeleteMethod::RecycleBin,
                    remember: false,
                })
            }),
            ("remove_summary", |app| {
                app.remove_summary = Some(crate::app::RemoveSummary {
                    succeeded: 1,
                    nuked: 0,
                    failed: Vec::new(),
                    method: gametrimmer_core::settings::DeleteMethod::RecycleBin,
                    expected_bytes: 0,
                    freed_bytes: 0,
                    recycled_pending_bytes: 0,
                })
            }),
        ];

        for (name, open) in modals {
            let mut test = tree_with_cursor();
            open(test.app_mut());
            test.run();

            test.press(egui::Key::ArrowDown);

            assert_eq!(
                test.app().tree_cursor,
                Some(0),
                "the tree handled ArrowDown while {name} was open",
            );
        }
    }

    /// A tree with everything checked under a named profile, so any edit is
    /// visibly a departure from it.
    fn tree_under_profile(profile: SelectionProfile) -> UiTest {
        let mut test = tree_with_cursor();
        test.app_mut().settings.selection_profile = profile;
        test.run();
        test
    }

    /// The audit's §5.5: the picker said "Balanced" while the checkboxes had
    /// been hand-edited into something else. `SelectionProfile::Custom` was
    /// documented as the state for exactly this and nothing ever set it.
    ///
    /// Driven through the keyboard toggle specifically, because that is one
    /// of the paths a per-call-site hook is most likely to miss - it mutates
    /// `findings` from `handle_keyboard`, nowhere near the checkbox code.
    #[test]
    fn a_keyboard_toggle_moves_the_profile_to_custom() {
        let mut test = tree_under_profile(SelectionProfile::Balanced);

        test.press(egui::Key::Space);

        assert_eq!(
            test.app().settings.selection_profile,
            SelectionProfile::Custom,
            "hand-editing the selection must stop the picker claiming a policy",
        );
    }

    /// The control: merely looking at the tree is not an edit. Without this,
    /// the test above would also pass if the profile flipped to Custom on
    /// every frame.
    #[test]
    fn navigating_without_editing_leaves_the_profile_alone() {
        let mut test = tree_under_profile(SelectionProfile::Balanced);

        test.press(egui::Key::ArrowDown);
        test.press(egui::Key::ArrowUp);
        test.run();

        assert_eq!(
            test.app().settings.selection_profile,
            SelectionProfile::Balanced,
            "moving the cursor is not a selection edit",
        );
    }

    /// Switching the profile re-checks the findings on purpose, and that must
    /// not immediately read back as a hand-edit - otherwise picking
    /// "Cautious" would snap straight to "Custom" on the next frame.
    #[test]
    fn choosing_a_profile_does_not_read_back_as_a_hand_edit() {
        let mut test = tree_under_profile(SelectionProfile::Balanced);

        test.app_mut()
            .set_selection_profile(SelectionProfile::Cautious);
        test.run();
        test.run();

        assert_eq!(
            test.app().settings.selection_profile,
            SelectionProfile::Cautious,
            "applying a profile is not a hand-edit",
        );
    }

    /// A tree seeded with two games on one disk, giving three collapsed rows
    /// in a known order: disk, "Test Game 0", "Test Game 1" (games sort by
    /// size, then by name - the two are the same size).
    fn tree_of_three_rows() -> UiTest {
        let mut test = UiTest::new(show);
        test.seed_findings();
        test.run();
        test
    }

    /// A point inside `row` that is past the row's name and short of the
    /// right-aligned columns - the dead width GT-32 is about.
    fn empty_space_right_of(name_rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(name_rect.max.x + 24.0, name_rect.center().y)
    }

    /// GT-32: the row, not just its name, is the click target.
    #[test]
    fn a_click_beside_the_name_places_the_cursor_on_that_row() {
        let mut test = tree_of_three_rows();
        let name = i18n::quoted(test.app().lang(), "Test Game 1");
        let target = empty_space_right_of(test.rect_of(&name));

        test.click_at(target);

        assert_eq!(
            test.app().tree_cursor,
            Some(2),
            "clicking the blank part of the third row did not select it",
        );
    }

    /// The control: the widened target is per row, not one target for the
    /// whole list. Without this, the test above would also pass if any click
    /// anywhere parked the cursor on a fixed row.
    #[test]
    fn each_row_claims_only_its_own_width() {
        let mut test = tree_of_three_rows();
        let disk = i18n::disk_label(test.app().lang(), "C:");
        let target = empty_space_right_of(test.rect_of(&disk));

        test.click_at(target);

        assert_eq!(
            test.app().tree_cursor,
            Some(0),
            "clicking beside the disk name selected some other row",
        );
    }

    /// The safety half of GT-32: a wide, easily-hit target moves the cursor
    /// and nothing else. If a row click also ticked the row's checkbox, a
    /// stray click anywhere on the panel would mark files for deletion.
    #[test]
    fn a_row_click_never_marks_anything_for_deletion() {
        let mut test = tree_of_three_rows();
        let before: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();
        let name = i18n::quoted(test.app().lang(), "Test Game 1");
        let target = empty_space_right_of(test.rect_of(&name));

        test.click_at(target);

        let after: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();
        assert_eq!(
            before, after,
            "clicking a row changed what is marked for deletion",
        );
    }

    /// The regression the full-width rect could cause: it is registered
    /// before the row's own widgets precisely so they keep their clicks. If
    /// that ordering ever inverts, the checkboxes go dead while every test
    /// above still passes.
    #[test]
    fn the_row_background_does_not_swallow_a_checkbox_click() {
        let mut test = tree_of_three_rows();
        assert!(
            test.app().findings.iter().all(|f| f.selected),
            "the seeded findings should start fully selected",
        );

        // Checkboxes render in row order; index 1 is the first game row.
        let checkbox = test.nth_checkbox_rect(1);
        test.click_at(checkbox.center());

        assert_eq!(
            test.app().findings.iter().filter(|f| f.selected).count(),
            1,
            "the game row's checkbox no longer unticks its own findings",
        );
    }

    /// Space toggles the selection of the row under the cursor, so it is the
    /// destructive half of the same gap: a keystroke behind a dialog could
    /// change what a pending delete is about to remove.
    #[test]
    fn space_does_not_toggle_selection_behind_a_dialog() {
        let mut test = tree_with_cursor();
        test.app_mut().show_settings = true;
        test.run();
        let before: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();

        test.press(egui::Key::Space);

        let after: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();
        assert_eq!(
            before, after,
            "Space changed the selection while the settings dialog was open",
        );
    }
}
