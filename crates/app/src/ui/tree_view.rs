//! Central panel: the branch -> game -> category -> folder/file findings tree
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
//! branch/game/category/folder), and handed to
//! `egui::ScrollArea::show_rows`, which only asks us to actually lay out
//! the handful of rows currently scrolled into view. The flattening walk
//! itself is cheap: it produces a `Vec` of small `Copy` enum values (a few
//! indices each, no strings, no cloned `Vec<usize>`), so even at 100k rows
//! it's a fast, allocation-light pass - the expensive part (widget layout,
//! text shaping, formatting) only ever runs for the visible range.
//!
//! Top-level branches and categories default to open; game and folder nodes
//! default to closed, so a fresh scan shows a compact per-game summary
//! instead of a wall of files - details stay one click (or `→`) away. The open/closed
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
//! cursor there (whole-row interaction).
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
//! A click on the row moves the cursor there and, on a row that has children,
//! expands or collapses it (MT-E07).
//!
//! Note what a row click deliberately does *not* do: it never ticks the row's
//! checkbox. In a tool that deletes files those are two different things, and
//! a stray click on a wide target must not be able to mark anything for
//! removal - marking stays on the checkbox and on the explicit `Space`/`Enter`
//! toggle.

use std::collections::HashMap;
use std::path::Path;

use eframe::egui;

use crate::app::GameTrimmerApp;
use crate::i18n::{self, Lang};
use crate::model::{
    self, category_display, category_ui_key, format_size, group_selection_state, is_orphan_branch,
    set_group_selection, toggle_group, DisplayCategory, FindingItem, GameNode, GroupAxis,
    SortColumn, TopGroup, TopKey, TreeNode, TreeSort, REVIEW_CONFIDENCE_THRESHOLD,
};
use crate::search::SearchIndex;
use crate::ui::highlight::{self, Part};
use crate::ui::row_actions;

/// Horizontal indent per nesting level (top-level branch = 0, game = 1,
/// category = 2, folder = 3, orphan file = 3, folder member = 4).
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

/// Arrows a column heading carries while the tree is ordered by it.
///
/// Deliberately not the \u{25b2}/\u{25bc} pair: \u{25bc} is already the glyph
/// every expandable row uses for "open", and one character meaning both
/// "expanded" and "descending" on the same screen is a puzzle for the reader,
/// not a saving.
const SORT_ASCENDING_GLYPH: &str = "\u{2191}";
const SORT_DESCENDING_GLYPH: &str = "\u{2193}";

/// What a sortable heading carries while the tree is *not* ordered by it.
///
/// Every sortable heading is marked, always - a heading that only reveals
/// itself as a control once the pointer is over it is a control nobody knows
/// to look for. The double-headed arrow says "this can go either way" without
/// claiming a direction the tree is not currently in.
const SORT_AVAILABLE_GLYPH: &str = "\u{2195}";

/// One visible row in the flattened tree, referencing the source tree only
/// by index - cheap to push into a per-frame `Vec` for tens of thousands of
/// rows.
#[derive(Debug, Clone, Copy)]
enum Row {
    /// A top-level branch: what it stands for depends on the active grouping
    /// axis (see `model::TopKey`), which is why the field is `d` rather than
    /// anything disk-shaped.
    Top {
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

/// A row plus how deep it is drawn.
///
/// The indent is carried rather than derived from the row's kind, because the
/// active axis can fold whole levels away (see [`DrawnLevels`]) and a game row
/// under the flat axis is not at the depth a game row is under the disk axis.
/// Storing it also collapses two things that used to have to agree by hand:
/// the indent the row is painted at, and the depth `<-` walks up through.
#[derive(Debug, Clone, Copy)]
struct VisibleRow {
    row: Row,
    indent: usize,
}

/// Which of the model's three header levels the active axis actually draws.
///
/// `model::build_tree` produces the same branch -> game -> category -> node
/// shape whatever the axis; what changes is which of those headings still say
/// something. Under [`GroupAxis::Category`] the per-game category row would
/// only repeat the branch heading one indent in, and under
/// [`GroupAxis::Flat`] every heading above the file is one the user asked to
/// be rid of.
///
/// Folding at draw time rather than at build time is deliberate: the export,
/// the totals and the sort all walk the tree, and none of them should have to
/// know which headings the screen is currently showing.
#[derive(Debug, Clone, Copy)]
struct DrawnLevels {
    top: bool,
    game: bool,
    category: bool,
}

impl DrawnLevels {
    fn of(axis: GroupAxis) -> Self {
        match axis {
            GroupAxis::Disk | GroupAxis::Launcher | GroupAxis::Library => Self {
                top: true,
                game: true,
                category: true,
            },
            GroupAxis::Category => Self {
                top: true,
                game: true,
                category: false,
            },
            GroupAxis::Flat => Self {
                top: false,
                game: false,
                category: false,
            },
        }
    }

    /// Whether this game row says anything its branch heading has not.
    ///
    /// The orphan pseudo-game (`model::ORPHAN_GAME_ID`) exists to give residue
    /// a home under a level that otherwise means "game". Under the category
    /// axis that residue's home is already the "Orphaned residue" branch, so
    /// the row would repeat that heading verbatim one indent in - the same
    /// redundancy that folds the category level away there.
    fn draws_game(self, axis: GroupAxis, game_id: i64) -> bool {
        self.game && !(axis == GroupAxis::Category && is_orphan_branch(game_id))
    }

    /// The indent a game row sits at: right under the branch heading when
    /// there is one, at the margin when there is not.
    fn game_indent(self) -> usize {
        usize::from(self.top)
    }

    fn category_indent(self) -> usize {
        self.game_indent() + usize::from(self.game)
    }

    /// Where folders and standalone files sit - under the category row if it
    /// is drawn, otherwise under whatever the deepest drawn heading was.
    fn node_indent(self, axis: GroupAxis, game_id: i64) -> usize {
        self.game_indent()
            + usize::from(self.draws_game(axis, game_id))
            + usize::from(self.category)
    }
}

pub fn show(app: &mut GameTrimmerApp, ui: &mut egui::Ui) {
    let lang = app.lang();

    // Selection is recorded exactly at its mutation sites instead of hashing
    // every finding before and after every frame. With a large result set the
    // old two full-list fingerprints were measurable UI work even while the
    // user merely scrolled.
    let selection_changed = std::cell::Cell::new(false);
    // Outlives the row pass on purpose - see `RowCtx::keep_request`.
    let keep_request = std::cell::Cell::new(None);

    egui::CentralPanel::default().show(ui, |ui| {
        // Before the first scan - and before the disclaimer is accepted -
        // this space carries the introduction (first-run onboarding) rather than one line of
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

        // plan-action filtering plan cards ride at the top of this panel, directly above the
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

        // Applied straight away rather than deferred to the end of the frame:
        // nothing has borrowed the tree yet at this point, and reordering it
        // now means the rows flattened below are already the ones the click
        // asked for, instead of the screen lagging the header by a frame.
        if let Some(next) = show_column_headers(ui, lang, app.tree_sort) {
            app.set_tree_sort(next);
        }
        ui.separator();

        let row_height = ui.spacing().interact_size.y;
        let row_stride = row_height + ui.spacing().item_spacing.y;

        // Keyboard input is handled before rendering so the resulting
        // cursor/toggle/scroll changes are visible the same frame. Modals
        // own the keyboard while open - see `GameTrimmerApp::any_modal_open`,
        // which is the single list of them (this used to enumerate three of
        // the five here and silently kept handling keys behind the settings
        // dialog and the clear-database confirmation).
        // One normal frame constructs the flattened list once. Keyboard
        // expansion/collapse is the sole path that can change structural
        // visibility before it is rendered, and only then do we rebuild it.
        let mut rows = build_visible_rows(
            &app.tree,
            &app.findings,
            app.tree_axis,
            &app.tree_toggles,
            app.tree_category_filter,
            &app.tree_search_index,
        );
        let mut scroll_override = None;
        if !app.any_modal_open() {
            let keyboard = handle_keyboard(app, ui, &rows, row_stride);
            scroll_override = keyboard.scroll_override;
            if keyboard.visibility_changed {
                rows = build_visible_rows(
                    &app.tree,
                    &app.findings,
                    app.tree_axis,
                    &app.tree_toggles,
                    app.tree_category_filter,
                    &app.tree_search_index,
                );
            }
        }
        if let Some(cursor) = app.tree_cursor {
            if cursor >= rows.len() {
                app.tree_cursor = rows.len().checked_sub(1);
            }
        }

        // A search or a category filter can hide every row of a tree that is
        // itself far from empty. Handing zero rows to the scroll area below
        // leaves a blank panel with nothing to explain it, which reads as
        // broken detection rather than as an empty result - the same reason
        // the unscanned window carries a hint instead of silence (MT-F05).
        if rows.is_empty() {
            ui.add_space(16.0);
            ui.label(i18n::strings(lang).search_no_matches);
            return;
        }

        // Disjoint field borrows: `tree` (read-only tree shape), `findings`
        // (checkbox mutation), `tree_toggles` (expand/collapse state) and
        // `tree_cursor` never alias each other, so this compiles without
        // cloning the tree.
        let tree = &app.tree;
        let findings = &mut app.findings;
        let toggles = &mut app.tree_toggles;
        let cursor = &mut app.tree_cursor;
        let ctx = RowCtx {
            axis: app.tree_axis,
            lang,
            descriptions: &app.descriptions,
            query: app.tree_search_index.query(),
            keep_request: &keep_request,
            selection_changed: &selection_changed,
            updated_games: &app.updated_games,
        };

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
                    ctx,
                );
            }
        });

        // Remembered for next frame's keyboard handling (PgUp/PgDn page
        // size, keeping the cursor scrolled into view).
        app.tree_scroll_offset = output.state.offset.y;
        app.tree_viewport_height = output.inner_rect.height();
    });

    // Apply this after the direct selection edits: dropping the kept row can
    // clear its own selection, which is a consequence of the exception rather
    // than something the user ticked.
    if let Some(index) = keep_request.take() {
        apply_keep_request(app, index);
    }
}

/// The header row naming the fixed columns, laid out with the same widths
/// as every data row so the columns visually line up. Every heading is also
/// the control that orders the tree by its column.
///
/// Returns the sort state the user's click asks for, or `None` if they did not
/// click a heading this frame. Reporting the request rather than applying it
/// keeps this function free of `GameTrimmerApp` - it can be reasoned about, and
/// tested, as "these four headings, this current order, this click".
fn show_column_headers(
    ui: &mut egui::Ui,
    lang: Lang,
    current: Option<TreeSort>,
) -> Option<Option<TreeSort>> {
    let s = i18n::strings(lang);
    // The four cells are laid out by sibling closures, so none of them can
    // hold a `&mut` to the answer. A `Cell` lets each of them write through a
    // shared borrow instead; at most one is clicked per frame.
    let clicked: std::cell::Cell<Option<Option<TreeSort>>> = std::cell::Cell::new(None);
    let record = |request: Option<Option<TreeSort>>| {
        if let Some(next) = request {
            clicked.set(Some(next));
        }
    };

    row_columns_with(
        ui,
        // Plain text, and the only heading in this row that is not a control:
        // a language tag exists on file rows alone, so ordering by it would
        // arrange four of the five levels by name and the fifth by tag. See
        // `model::SortColumn` for why that heading is not offered at all
        // rather than offered and quietly useless.
        |ui| {
            ui.add(egui::Label::new(egui::RichText::new(s.col_language).strong()).truncate());
        },
        |ui| record(header_cell(ui, s.col_files, SortColumn::Files, current, s)),
        |ui| record(header_cell(ui, s.col_size, SortColumn::Size, current, s)),
        |ui| {
            ui.add_space(4.0);
            record(header_cell(ui, s.col_name, SortColumn::Name, current, s));
        },
    );

    clicked.get()
}

/// Draws one column heading and reports the sort state a click on it asks for.
///
/// The heading carries a direction arrow only while the tree is actually
/// ordered by it, so the row states which column is in force instead of
/// leaving the user to infer it from the data.
fn header_cell(
    ui: &mut egui::Ui,
    title: &str,
    column: SortColumn,
    current: Option<TreeSort>,
    s: &i18n::Strings,
) -> Option<Option<TreeSort>> {
    let label = match current.filter(|sort| sort.column == column) {
        Some(sort) if sort.descending => format!("{title} {SORT_DESCENDING_GLYPH}"),
        Some(_) => format!("{title} {SORT_ASCENDING_GLYPH}"),
        None => format!("{title} {SORT_AVAILABLE_GLYPH}"),
    };
    // Frameless, so the row still reads as a table heading rather than as four
    // buttons stacked above the tree - but a button all the same, so it
    // highlights under the pointer and "this can be clicked" is discoverable
    // without having to wait for the tooltip.
    let response = ui
        .add(egui::Button::new(egui::RichText::new(label).strong()).frame(false))
        .on_hover_text(s.col_sort_hint);
    response.clicked().then(|| next_sort(current, column))
}

/// The sort state one click on `column` moves to, cycling
/// ordered -> reversed -> the tree's own order (`None`), then round again.
///
/// The third step is the reason this is a cycle rather than a toggle. `None` is
/// not "unsorted": it is the order `model::build_tree` designed, which mixes
/// four different keys across the levels and which no column-and-direction pair
/// reproduces. Without a way back, the first click on any heading would destroy
/// it for the rest of the session.
///
/// Which direction a column opens in differs by what it holds: the numeric
/// columns start descending because "biggest first" is the question being
/// asked of them, while the textual ones start at A. Opening a size sort at the
/// smallest files would spend the click that matters on the answer nobody
/// wanted.
fn next_sort(current: Option<TreeSort>, column: SortColumn) -> Option<TreeSort> {
    let opens_descending = matches!(column, SortColumn::Files | SortColumn::Size);
    match current {
        Some(sort) if sort.column == column && sort.descending == opens_descending => {
            Some(TreeSort {
                column,
                descending: !opens_descending,
            })
        }
        Some(sort) if sort.column == column => None,
        _ => Some(TreeSort {
            column,
            descending: opens_descending,
        }),
    }
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
    row_columns_with(
        ui,
        |ui| {
            ui.add(egui::Label::new(lang).truncate());
        },
        |ui| {
            ui.add(egui::Label::new(count).truncate());
        },
        |ui| {
            ui.add(egui::Label::new(size).truncate());
        },
        left,
    );
}

/// [`row_columns`] with the three fixed-width cells drawn by closures instead
/// of being plain text.
///
/// Only the header row needs this - its cells are buttons, not labels. It
/// exists so the header keeps going through the same widths and the same
/// nested layouts as every data row: a second hand-written copy of this
/// arrangement is exactly how a table's heading drifts out of line with its
/// body one edit at a time.
fn row_columns_with(
    ui: &mut egui::Ui,
    lang: impl FnOnce(&mut egui::Ui),
    count: impl FnOnce(&mut egui::Ui),
    size: impl FnOnce(&mut egui::Ui),
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

/// One fixed-width, right-aligned cell of [`row_columns_with`].
fn right_cell(ui: &mut egui::Ui, width: f32, content: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.set_min_width(width);
            content(ui);
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
/// branch/game/category/folder, and collects every structurally-visible row.
/// O(number of rows produced): closed subtrees are never descended into, so
/// this stays cheap even when the tree holds tens of thousands of findings.
/// Whether a game has any node under the active plan-card filter (plan-action filtering).
/// `None` filter matches everything.
fn game_matches_filter(game: &crate::model::GameNode, filter: Option<DisplayCategory>) -> bool {
    match filter {
        None => true,
        Some(category) => game
            .categories
            .iter()
            .any(|node| node.category == Some(category)),
    }
}

/// Whether a top-level branch has any game with a node under the active filter
/// (plan-action filtering).
fn top_matches_filter(top_group: &TopGroup, filter: Option<DisplayCategory>) -> bool {
    top_group
        .games
        .iter()
        .any(|game| game_matches_filter(game, filter))
}

/// Whether a game still has content under the active name search (name search).
///
/// Real games answer this in O(1) from the pre-built id set. A synthetic node
/// cannot: every top-level branch's orphan node shares the one
/// [`is_orphan_branch`] sentinel id, so the id-keyed answer would light up all
/// of them as soon as any one matched, and the flat axis's node
/// (`model::FLAT_GAME_ID`) is not in the index at all, so it would answer "no"
/// for every query. Both scan their own indices instead - which the flat axis
/// never actually reaches, since its game level is not drawn and so is never
/// tested.
fn game_matches_search(game: &GameNode, search: &SearchIndex) -> bool {
    if !search.is_active() {
        return true;
    }
    if model::is_real_game(game.game_id) {
        search.game_matches(game.game_id)
    } else {
        search.any_matches(&game.all_indices)
    }
}

fn build_visible_rows(
    tree: &[TopGroup],
    findings: &[FindingItem],
    axis: GroupAxis,
    toggles: &HashMap<String, bool>,
    filter: Option<DisplayCategory>,
    search: &SearchIndex,
) -> Vec<VisibleRow> {
    let mut rows = Vec::new();
    let levels = DrawnLevels::of(axis);
    // With no category level and no category branch, nothing above a file can
    // answer the plan-card filter, so each file answers for itself. That is
    // only the flat axis: the category axis folds the category row away too,
    // but there the *branch* is the category and the filter is settled one
    // level higher than usual rather than one level lower.
    let filter_per_file = axis == GroupAxis::Flat;

    for (d, top_group) in tree.iter().enumerate() {
        // Under a plan-card filter or a name search, a branch (and each game)
        // with nothing left to show is skipped entirely rather than shown as an
        // empty header, so the "View" action lands on exactly that category's
        // findings and a search shows only branches that contain a hit. A level
        // that is not drawn is not tested either - there is no header to
        // suppress, and the rows below answer for themselves.
        if levels.top {
            if !top_matches_filter(top_group, filter) {
                continue;
            }
            if !top_group
                .games
                .iter()
                .any(|game| game_matches_search(game, search))
            {
                continue;
            }
            rows.push(VisibleRow {
                row: Row::Top { d },
                indent: 0,
            });
            if !is_open(toggles, &top_key(&top_group.key), true) {
                continue;
            }
        }

        for (g, game) in top_group.games.iter().enumerate() {
            let draws_game = levels.draws_game(axis, game.game_id);
            if draws_game {
                if !game_matches_filter(game, filter) || !game_matches_search(game, search) {
                    continue;
                }
                rows.push(VisibleRow {
                    row: Row::Game { d, g },
                    indent: levels.game_indent(),
                });
                if !is_open(toggles, &game_key(&top_group.key, game.game_id), false) {
                    continue;
                }
            }
            let node_indent = levels.node_indent(axis, game.game_id);

            for (c, category_node) in game.categories.iter().enumerate() {
                if levels.category {
                    if filter.is_some_and(|category| category_node.category != Some(category)) {
                        continue;
                    }
                    // Only reached for an expanded game, so the per-item scans
                    // from here down are bounded by what the user has opened -
                    // never by the size of the whole result set.
                    if search.is_active() && !search.any_matches(&category_node.all_indices) {
                        continue;
                    }
                    rows.push(VisibleRow {
                        row: Row::Category { d, g, c },
                        indent: levels.category_indent(),
                    });
                    let cat_key =
                        category_key(&top_group.key, game.game_id, category_node.category);
                    if !is_open(toggles, &cat_key, true) {
                        continue;
                    }
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
                            rows.push(VisibleRow {
                                row: Row::Folder { d, g, c, n },
                                indent: node_indent,
                            });
                            let folder_key = folder_key(
                                &top_group.key,
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
                                rows.push(VisibleRow {
                                    row: Row::File {
                                        d,
                                        g,
                                        c,
                                        n,
                                        member: Some(member),
                                    },
                                    indent: node_indent + 1,
                                });
                            }
                        }
                        TreeNode::File { index } => {
                            if search.is_active() && !search.item_matches(*index) {
                                continue;
                            }
                            if filter_per_file
                                && filter.is_some_and(|category| {
                                    findings[*index].row.display_category() != category
                                })
                            {
                                continue;
                            }
                            rows.push(VisibleRow {
                                row: Row::File {
                                    d,
                                    g,
                                    c,
                                    n,
                                    member: None,
                                },
                                indent: node_indent,
                            });
                        }
                    }
                }
            }
        }
    }

    rows
}

/// Stable, collision-free key for a top-level branch row's expand/collapse
/// state.
///
/// Every key below is built on top of this one, and `TopKey::collapse_key`
/// carries the grouping axis - so the whole keyspace is namespaced per axis.
/// Without that, "disk E: is open" and "library E:\Games is open" would be the
/// same key, and the expand state of one axis would leak into the next
/// (GT-35's second pitfall).
fn top_key(key: &TopKey) -> String {
    format!("d|{}", key.collapse_key())
}

/// Stable, collision-free key for a game row's expand/collapse state.
fn game_key(top: &TopKey, game_id: i64) -> String {
    format!("g|{}|{game_id}", top.collapse_key())
}

/// The short key naming a category node, including the flat axis's
/// category-less one (`model::CategoryNode::category`). Only ever part of a
/// collapse key, so it needs to be stable and distinct - not readable.
fn category_node_key(category: Option<DisplayCategory>) -> &'static str {
    match category {
        Some(category) => category_ui_key(category),
        None => "*",
    }
}

/// Stable, collision-free key for a category row's expand/collapse state.
fn category_key(top: &TopKey, game_id: i64, category: Option<DisplayCategory>) -> String {
    format!(
        "c|{}|{game_id}|{}",
        top.collapse_key(),
        category_node_key(category)
    )
}

/// Stable, collision-free key for a folder row's expand/collapse state.
fn folder_key(
    top: &TopKey,
    game_id: i64,
    category: Option<DisplayCategory>,
    group_dir: &str,
) -> String {
    format!(
        "f|{}|{game_id}|{}|{group_dir}",
        top.collapse_key(),
        category_node_key(category)
    )
}

/// Whether a node keyed by `key` is currently open: an explicit user choice
/// if present in `toggles`, otherwise the node kind's default.
fn is_open(toggles: &HashMap<String, bool>, key: &str, default_open: bool) -> bool {
    toggles.get(key).copied().unwrap_or(default_open)
}

/// Index (into `rows`) of the closest preceding row drawn shallower than this
/// one - the cursor row's structural parent.
///
/// Reads the indent the row is actually painted at rather than recomputing a
/// depth from its kind, so `<-` can never walk to a different place than the
/// screen shows. That matters once an axis folds levels away: under the
/// category axis a folder row's parent is the game row two indents up on the
/// disk axis and one indent up here.
fn parent_row_index(rows: &[VisibleRow], index: usize) -> Option<usize> {
    let indent = rows[index].indent;
    (0..index).rev().find(|&i| rows[i].indent < indent)
}

/// The expand/collapse toggle key and default-open state of a row, if the
/// row is expandable at all.
fn row_toggle_key(tree: &[TopGroup], row: Row) -> Option<(String, bool)> {
    match row {
        Row::Top { d } => Some((top_key(&tree[d].key), true)),
        Row::Game { d, g } => Some((game_key(&tree[d].key, tree[d].games[g].game_id), false)),
        Row::Category { d, g, c } => {
            let top_group = &tree[d];
            let game = &top_group.games[g];
            Some((
                category_key(&top_group.key, game.game_id, game.categories[c].category),
                true,
            ))
        }
        Row::Folder { d, g, c, n } => {
            let top_group = &tree[d];
            let game = &top_group.games[g];
            let category_node = &game.categories[c];
            let TreeNode::Folder { group_dir, .. } = &category_node.nodes[n] else {
                return None;
            };
            Some((
                folder_key(
                    &top_group.key,
                    game.game_id,
                    category_node.category,
                    group_dir,
                ),
                false,
            ))
        }
        // A file row is never expandable: GameTrimmer has no way to look
        // inside a container, and every non-container file is a leaf.
        Row::File { .. } => None,
    }
}

/// Flat `findings` index of a file row.
fn file_row_index(
    tree: &[TopGroup],
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
fn toggle_row_selection(tree: &[TopGroup], findings: &mut [FindingItem], row: Row) -> bool {
    match row {
        Row::Top { d } => toggle_group(findings, &tree[d].all_indices),
        Row::Game { d, g } => toggle_group(findings, &tree[d].games[g].all_indices),
        Row::Category { d, g, c } => {
            toggle_group(findings, &tree[d].games[g].categories[c].all_indices)
        }
        Row::Folder { d, g, c, n } => {
            if let TreeNode::Folder { item_indices, .. } = &tree[d].games[g].categories[c].nodes[n]
            {
                toggle_group(findings, item_indices)
            } else {
                false
            }
        }
        Row::File { d, g, c, n, member } => {
            let index = file_row_index(tree, d, g, c, n, member);
            if findings[index].row.individually_selectable() {
                findings[index].selected = !findings[index].selected;
                true
            } else {
                false
            }
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

/// The two independent results of keyboard handling. Moving a cursor can
/// request a scroll without changing the flattened rows; expanding or
/// collapsing needs a single rebuild before this frame is painted.
#[derive(Default)]
struct KeyboardResult {
    scroll_override: Option<f32>,
    visibility_changed: bool,
}

/// Handles keyboard navigation over the flattened row list. Returns a new
/// scroll offset when the view must jump (PgUp/PgDn scrolling, or keeping
/// the moved cursor visible); `None` leaves the scroll position alone.
fn handle_keyboard(
    app: &mut GameTrimmerApp,
    ui: &egui::Ui,
    rows: &[VisibleRow],
    row_stride: f32,
) -> KeyboardResult {
    if rows.is_empty() {
        return KeyboardResult::default();
    }
    // A focused widget (button, checkbox, ...) owns the keyboard - don't
    // fight it over Space/Enter/arrows.
    if ui.ctx().memory(|memory| memory.focused().is_some()) {
        return KeyboardResult::default();
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
            return KeyboardResult {
                scroll_override: Some(app.tree_scroll_offset + app.tree_viewport_height),
                ..KeyboardResult::default()
            };
        }
        if keys.page_up {
            return KeyboardResult {
                scroll_override: Some((app.tree_scroll_offset - app.tree_viewport_height).max(0.0)),
                ..KeyboardResult::default()
            };
        }
        if keys.home {
            return KeyboardResult {
                scroll_override: Some(0.0),
                ..KeyboardResult::default()
            };
        }
        if keys.end {
            return KeyboardResult {
                scroll_override: Some(rows.len() as f32 * row_stride),
                ..KeyboardResult::default()
            };
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

    let mut visibility_changed = false;
    if let Some(current) = cursor {
        let row = rows[current.min(last)].row;
        if keys.toggle_select {
            toggle_row_selection(&app.tree, &mut app.findings, row);
        }
        if keys.expand || keys.collapse {
            match row_toggle_key(&app.tree, row) {
                Some((key, default_open)) => {
                    let open = is_open(&app.tree_toggles, &key, default_open);
                    if keys.expand && !open {
                        app.tree_toggles.insert(key, true);
                        visibility_changed = true;
                    } else if keys.collapse && open {
                        app.tree_toggles.insert(key, false);
                        visibility_changed = true;
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
        let Some(current) = cursor else {
            return KeyboardResult {
                scroll_override: None,
                visibility_changed,
            };
        };
        let top = current as f32 * row_stride;
        let bottom = top + row_stride;
        let view_height = app.tree_viewport_height.max(row_stride);
        if top < app.tree_scroll_offset {
            return KeyboardResult {
                scroll_override: Some(top),
                visibility_changed,
            };
        }
        if bottom > app.tree_scroll_offset + view_height {
            return KeyboardResult {
                scroll_override: Some(bottom - view_height),
                visibility_changed,
            };
        }
    }
    KeyboardResult {
        scroll_override: None,
        visibility_changed,
    }
}

/// What every row of one frame needs and no row owns: the axis the tree is cut
/// along, the interface language, and the search query whose matches the names
/// tint.
///
/// One `Copy` bundle rather than three parameters threaded through five row
/// functions - they always travel together, and the row functions are already
/// long in the arguments.
#[derive(Debug, Clone, Copy)]
struct RowCtx<'a> {
    axis: GroupAxis,
    lang: Lang,
    /// Resolves each row's stored (English) description into the current
    /// language - see `worker::descriptions`.
    descriptions: &'a crate::worker::descriptions::Descriptions,
    /// Folded exactly as [`SearchIndex`] folded it, or `""` when no search is
    /// in effect.
    query: &'a str,
    /// Where a "never touch this" click parks the row index it was made on.
    ///
    /// A `Cell` because the row pass holds `&mut app.findings` for the whole
    /// scroll area, so the menu cannot reach `app` to do the write itself -
    /// and must not, since writing the pack mid-pass would mutate the very
    /// list being iterated. The click records; [`show`] acts once every
    /// borrow is released. Last click of a frame wins, which is the only
    /// thing that can happen anyway: the menu closes on click.
    keep_request: &'a std::cell::Cell<Option<usize>>,
    /// Set by each direct checkbox/context-menu mutation, avoiding an O(N)
    /// scan of selection state around every rendered frame.
    selection_changed: &'a std::cell::Cell<bool>,
    /// Map of recently updated games from background monitoring.
    updated_games: &'a std::collections::HashMap<String, String>,
}

#[allow(clippy::too_many_arguments)]
fn show_row(
    ui: &mut egui::Ui,
    tree: &[TopGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    visible: VisibleRow,
    row_index: usize,
    ctx: RowCtx<'_>,
) {
    let VisibleRow { row, indent } = visible;
    let row_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
    );

    // whole-row interaction: the whole row is a click target. Registered here, before the
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

        // The same click also expands or collapses an expandable row (MT-E07).
        // Moving the cursor is all it used to do, and the cursor is a thin
        // highlight - so on the wide empty part of a group row the click read
        // as nothing happening at all. Folding is what the row visibly offers
        // and it is undone by clicking again; ticking, per this module's
        // header, stays out of reach of a stray click. File rows have nothing
        // to fold and keep cursor-only behaviour.
        if let Some((key, default_open)) = row_toggle_key(tree, row) {
            let open = is_open(toggles, &key, default_open);
            toggles.insert(key, !open);
        }
    }

    match row {
        Row::Top { d } => show_top_row(
            ui, tree, findings, toggles, cursor, d, row_index, indent, ctx,
        ),
        Row::Game { d, g } => show_game_row(
            ui, tree, findings, toggles, cursor, d, g, row_index, indent, ctx,
        ),
        Row::Category { d, g, c } => show_category_row(
            ui, tree, findings, toggles, cursor, d, g, c, row_index, indent, ctx,
        ),
        Row::Folder { d, g, c, n } => show_folder_row(
            ui, tree, findings, toggles, cursor, d, g, c, n, row_index, indent, ctx,
        ),
        Row::File { d, g, c, n, member } => show_file_row(
            ui, tree, findings, cursor, d, g, c, n, member, row_index, indent, ctx,
        ),
    }
}

/// The folder a top-level branch stands for on disk, if it stands for one at
/// all.
///
/// A disk row means its root; a library row means the library root. The rest
/// mean no one folder - "Steam" is not a directory and the same launcher's
/// games can be spread across several; a category spans every game there is -
/// so they get no shell actions rather than a made-up path.
fn top_shell_target(key: &TopKey) -> Option<ShellTarget> {
    match key {
        TopKey::Disk(disk) => Some(ShellTarget::Folder(disk_root_path(disk))),
        TopKey::Library(root) => Some(ShellTarget::Folder(row_actions::windows_path_string(root))),
        TopKey::Launcher(_) | TopKey::Category(_) | TopKey::Flat | TopKey::Unattributed(_) => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn show_top_row(
    ui: &mut egui::Ui,
    tree: &[TopGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    row_index: usize,
    indent: usize,
    ctx: RowCtx<'_>,
) {
    let lang = ctx.lang;
    let top_group = &tree[d];
    let key = top_key(&top_group.key);
    // Not tinted by the search: a disk letter, a launcher, a library root and a
    // category name are none of them fields the index reads, so a query can
    // never be the reason this heading is on screen.
    let name = egui::RichText::new(i18n::top_group_label(lang, &top_group.key)).strong();
    let response = show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        true,
        indent,
        &top_group.all_indices,
        top_group.total_bytes,
        name,
        lang,
        ctx.selection_changed,
    );
    let target = top_shell_target(&top_group.key);
    let response = match &target {
        Some(target) => response.on_hover_text(target.path().to_string()),
        None => response,
    };
    row_context_menu(&response, lang, target, |ui| {
        if ui
            .button(i18n::select_all_in_group(lang, &top_group.key))
            .clicked()
        {
            if set_group_selection(findings, &top_group.all_indices, true) {
                ctx.selection_changed.set(true);
            }
            ui.close();
        }
        if ui
            .button(i18n::deselect_all_in_group(lang, &top_group.key))
            .clicked()
        {
            if set_group_selection(findings, &top_group.all_indices, false) {
                ctx.selection_changed.set(true);
            }
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn show_game_row(
    ui: &mut egui::Ui,
    tree: &[TopGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    row_index: usize,
    indent: usize,
    ctx: RowCtx<'_>,
) {
    let lang = ctx.lang;
    let top_group = &tree[d];
    let game = &top_group.games[g];
    let key = game_key(&top_group.key, game.game_id);
    // The orphan branch (orphan-residue safety) has no real game name - render a localized
    // "orphaned residue" label instead of a quoted game title. Computed live
    // off the sentinel id, so it follows the current UI language even though
    // the stored `game_name` is empty.
    let label = game_branch_label(lang, game);
    let unclaimed = !model::is_pseudo_branch(game.game_id)
        && !game_has_launcher_id(findings, &game.all_indices);
    // Anti-cheat protection is a per-*game* verdict (see
    // `FindingRow::anti_cheat_protected`), copied onto every one of the
    // game's findings identically - so, exactly like the `is_updated` lookup
    // below, the first surviving finding already tells the whole game's
    // story.
    let is_anti_cheat = !model::is_pseudo_branch(game.game_id)
        && game
            .all_indices
            .first()
            .and_then(|&idx| findings.get(idx))
            .map(|item| item.row.anti_cheat_protected)
            .unwrap_or(false);
    let name = if model::is_pseudo_branch(game.game_id) {
        // A UI string, not a name the search index holds: drawn as decoration
        // so a query that happens to occur in it tints nothing.
        highlight::strong_name(ui, &[Part::decoration(label.as_str())], ctx.query)
    } else {
        let (open, close) = i18n::quote_marks(lang);
        let mut parts = vec![
            Part::decoration(open),
            Part::searched(label.as_str()),
            Part::decoration(close),
        ];
        if unclaimed {
            // Decoration, not searched text: a query matching the marker must
            // not tint anything, and the marker is not part of the name.
            parts.push(Part::decoration(" ◇"));
        }
        if is_anti_cheat {
            parts.push(Part::decoration(" "));
            parts.push(Part::decoration(i18n::strings(lang).badge_anticheat_shield));
        }
        let is_updated = !model::is_pseudo_branch(game.game_id)
            && (ctx.updated_games.contains_key(&game.game_name)
                || game
                    .all_indices
                    .first()
                    .and_then(|&idx| findings.get(idx))
                    .and_then(|f| f.row.app_id.as_ref())
                    .map(|id| ctx.updated_games.contains_key(id))
                    .unwrap_or(false));
        if is_updated {
            parts.push(Part::decoration(" [🔄 Updated]"));
        }
        highlight::strong_name(ui, &parts, ctx.query)
    };
    let response = show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        false,
        indent,
        &game.all_indices,
        game.total_bytes,
        name,
        lang,
        ctx.selection_changed,
    );
    // The game's install dir, taken from any of its findings (they all share
    // it). Absent on the orphan branch (orphan-residue safety): its findings are residue from
    // different games, so there is no one folder the row could stand for.
    let target = if model::is_pseudo_branch(game.game_id) {
        None
    } else {
        install_dir_of(findings, &game.all_indices).map(ShellTarget::Folder)
    };
    let response = match &target {
        Some(target) => response.on_hover_text(target.path().to_string()),
        None => response,
    };
    // A game no launcher claims is not the same kind of row as the rest, and
    // the difference is not cosmetic: without a vendor id, everything keyed to
    // a launcher manifest is unavailable for it - and, since GT-21, so is
    // "never touch this file in this game", whose whole scope *is* the app id.
    // Marked rather than left to look identical: a feature that silently is
    // not there for some rows is indistinguishable from one that is broken.
    let response = if unclaimed {
        response.on_hover_text(i18n::strings(lang).game_without_launcher_id)
    } else {
        response
    };
    let response = if is_anti_cheat {
        response.on_hover_text(i18n::strings(lang).anticheat_shield_tooltip)
    } else {
        response
    };

    // Under the category axis this row holds one category's worth of the game,
    // not the game's whole contribution to the tree - so the plain "Select all
    // in {game}" would claim more than the click does. `game.all_indices` is
    // the same either way; only the sentence changes.
    let scoped_category = match (ctx.axis, &top_group.key) {
        (GroupAxis::Category, TopKey::Category(category)) => Some(*category),
        _ => None,
    };
    let (select, deselect) = match scoped_category {
        Some(category) => {
            let name = category_display(lang, category);
            (
                i18n::select_category_in_game(lang, name, &label),
                i18n::deselect_category_in_game(lang, name, &label),
            )
        }
        None => (
            i18n::select_all_in_game(lang, &label),
            i18n::deselect_all_in_game(lang, &label),
        ),
    };

    row_context_menu(&response, lang, target, |ui| {
        if ui.button(select).clicked() {
            if set_group_selection(findings, &game.all_indices, true) {
                ctx.selection_changed.set(true);
            }
            ui.close();
        }
        if ui.button(deselect).clicked() {
            if set_group_selection(findings, &game.all_indices, false) {
                ctx.selection_changed.set(true);
            }
            ui.close();
        }
    });
}

/// The label shown for a game node: a localized heading for each synthetic
/// branch - "orphaned residue" (orphan-residue safety) or the system and
/// launcher files that live outside every game - and otherwise the real
/// game's own name.
fn game_branch_label(lang: Lang, game: &GameNode) -> String {
    pseudo_branch_label(lang, game.game_id).unwrap_or_else(|| game.game_name.clone())
}

/// The heading for a synthetic branch, or `None` for a real game.
fn pseudo_branch_label(lang: Lang, game_id: i64) -> Option<String> {
    let strings = i18n::strings(lang);
    if is_orphan_branch(game_id) {
        Some(strings.orphan_branch_label.to_string())
    } else if model::is_system_branch(game_id) {
        Some(strings.system_branch_label.to_string())
    } else {
        None
    }
}

/// Every finding of `category` across all games of one top-level branch - the
/// target of the "select this category across the whole branch" bulk action.
fn category_indices_in_group(top_group: &TopGroup, category: DisplayCategory) -> Vec<usize> {
    top_group
        .games
        .iter()
        .flat_map(|game| {
            game.categories
                .iter()
                .filter(|category_node| category_node.category == Some(category))
                .flat_map(|category_node| category_node.all_indices.iter().copied())
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn show_category_row(
    ui: &mut egui::Ui,
    tree: &[TopGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    c: usize,
    row_index: usize,
    indent: usize,
    ctx: RowCtx<'_>,
) {
    let lang = ctx.lang;
    let top_group = &tree[d];
    let game = &top_group.games[g];
    let category_node = &game.categories[c];
    // `build_visible_rows` only emits this row while the category level is
    // drawn, and the one node that carries no category belongs to the axis
    // that folds the level away - so there is nothing to draw here rather than
    // a heading with no name.
    let Some(category) = category_node.category else {
        return;
    };
    let key = category_key(&top_group.key, game.game_id, category_node.category);
    let name = egui::RichText::new(category_display(lang, category));
    let response = show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        true,
        indent,
        &category_node.all_indices,
        category_node.total_bytes,
        name,
        lang,
        ctx.selection_changed,
    );
    // A category is a slice of one game, so it stands for that game's install
    // dir - the same folder its parent row opens.
    let target = if model::is_pseudo_branch(game.game_id) {
        None
    } else {
        install_dir_of(findings, &category_node.all_indices).map(ShellTarget::Folder)
    };
    let response = match &target {
        Some(target) => response.on_hover_text(target.path().to_string()),
        None => response,
    };

    row_context_menu(&response, lang, target, |ui| {
        let label = category_display(lang, category);
        if ui
            .button(i18n::select_category_in_group(lang, label, &top_group.key))
            .clicked()
        {
            let indices = category_indices_in_group(top_group, category);
            if set_group_selection(findings, &indices, true) {
                ctx.selection_changed.set(true);
            }
            ui.close();
        }
        if ui
            .button(i18n::deselect_category_in_group(
                lang,
                label,
                &top_group.key,
            ))
            .clicked()
        {
            let indices = category_indices_in_group(top_group, category);
            if set_group_selection(findings, &indices, false) {
                ctx.selection_changed.set(true);
            }
            ui.close();
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn show_folder_row(
    ui: &mut egui::Ui,
    tree: &[TopGroup],
    findings: &mut [FindingItem],
    toggles: &mut HashMap<String, bool>,
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    c: usize,
    n: usize,
    row_index: usize,
    indent: usize,
    ctx: RowCtx<'_>,
) {
    let lang = ctx.lang;
    let top_group = &tree[d];
    let game = &top_group.games[g];
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
        &top_group.key,
        game.game_id,
        category_node.category,
        group_dir,
    );
    // The trailing separator is part of the relative path the search reads, so
    // it is searchable text and not decoration: a query ending in `\` matched
    // this folder through it.
    let label = format!("{group_dir}\\");
    let name = highlight::name(ui, &[Part::searched(label.as_str())], ctx.query);
    let response = show_header_row(
        ui,
        findings,
        toggles,
        cursor,
        row_index,
        &key,
        false,
        indent,
        item_indices,
        *total_bytes,
        name,
        lang,
        ctx.selection_changed,
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

/// Renders one expandable header row shared by branch/game/category/folder
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
    name: impl Into<egui::WidgetText>,
    lang: Lang,
    selection_changed: &std::cell::Cell<bool>,
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
            // A click always works when something is selected (it clears the
            // group - deselecting ignores `bulk_selectable`, see
            // `set_group_selection`). With nothing selected, a click only
            // does something if there is at least one bulk-selectable row to
            // pick up; otherwise the whole group is anti-cheat protected (or
            // otherwise blocked) and every row needs a deliberate individual
            // tick, so the header has nothing left to do and must say so
            // instead of looking clickable and doing nothing.
            let has_bulk_selectable = indices
                .iter()
                .any(|&index| findings[index].row.bulk_selectable());
            let clickable = any_selected || has_bulk_selectable;
            let mut checked = all_selected;
            let checkbox = ui.add_enabled(
                clickable,
                egui::Checkbox::new(&mut checked, "").indeterminate(any_selected && !all_selected),
            );
            if checkbox.clicked() && toggle_group(findings, indices) {
                selection_changed.set(true);
            }
            if !clickable {
                // A disabled widget senses nothing, so the whole-row click
                // target underneath it (registered before this row's widgets -
                // see "Why the row is one click target") receives the click
                // instead and folds the group. The user aimed at a checkbox
                // and the tree collapsed under them.
                //
                // An explicitly interactive rect over the same area takes the
                // hit instead. It does nothing on click, which is what a
                // disabled control should do; the hover text says why. This
                // has to sense clicks, not just hovers - hovering alone does
                // not take the click away from the row.
                let blocked = ui.interact(
                    checkbox.rect,
                    ui.id().with(("group_checkbox_blocked", row_index)),
                    egui::Sense::click(),
                );
                blocked.on_hover_text(i18n::strings(lang).group_checkbox_disabled_hint);
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
            crate::logger::error(&format!("Failed to open Explorer: {err}"));
        }
        ui.close();
    }

    if matches!(target, ShellTarget::File(_)) && ui.button(s.ctx_open).clicked() {
        if let Err(err) = row_actions::open_file(path) {
            crate::logger::error(&format!("Failed to open file: {err}"));
        }
        ui.close();
    }

    if matches!(target, ShellTarget::File(_)) && ui.button(s.ctx_open_with).clicked() {
        let (program, args) = row_actions::open_with_args(path);
        if let Err(err) = row_actions::launch(program, &args) {
            crate::logger::error(&format!(
                "Failed to open the \"Open with...\" dialog: {err}"
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

/// The filesystem root a disk branch stands for: a drive letter row (`F:`)
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
/// Whether the launcher that owns this game gave it an id.
///
/// `None` means no launcher told us about this game at all: it was found by
/// the heuristic folder scan, or the user added its folder by hand. Every row
/// of one game carries the same `app_id`, so the first is enough - the same
/// reasoning as [`install_dir_of`] directly below.
fn game_has_launcher_id(findings: &[FindingItem], indices: &[usize]) -> bool {
    indices
        .first()
        .is_some_and(|&first| findings[first].row.app_id.is_some())
}

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
    tree: &[TopGroup],
    findings: &mut [FindingItem],
    cursor: &mut Option<usize>,
    d: usize,
    g: usize,
    c: usize,
    n: usize,
    member: Option<usize>,
    row_index: usize,
    indent: usize,
    ctx: RowCtx<'_>,
) {
    let lang = ctx.lang;
    let node = &tree[d].games[g].categories[c].nodes[n];
    // Owned pieces: `findings` is borrowed mutably below for the checkbox, so
    // nothing here may keep pointing into it.
    let (index, name_parts): (usize, Vec<Part<'static>>) = match (node, member) {
        (
            TreeNode::Folder {
                group_dir,
                item_indices,
                ..
            },
            Some(m),
        ) => {
            let index = item_indices[m];
            let row = &findings[index].row;
            let rel_path = &row.rel_path;
            // Members render under their folder header - repeating the
            // folder prefix on every line would defeat the grouping.
            let name = rel_path
                .strip_prefix(&format!("{group_dir}\\"))
                .unwrap_or(rel_path)
                .to_string();
            let parts = vec![Part::searched(name)];
            // The anti-cheat verdict is a per-*game* fact (see
            // `ui::tree_view::show_game_row`, which marks it once on the
            // game's own row), not drawn again here - see that function for
            // why.
            (index, parts)
        }
        (TreeNode::File { index }, None) => {
            let row = &findings[*index].row;
            // Under every other axis a heading above this row already says
            // which game the file belongs to. The flat axis has no headings at
            // all, so a bare relative path would leave a list of "loc_0.pak"
            // with nothing to tell one game's from another's - the row has to
            // carry that itself.
            let parts = if ctx.axis == GroupAxis::Flat {
                let (open, close) = i18n::quote_marks(lang);
                // On the orphan branch the leading name is a UI heading rather
                // than a game the search index knows, so it is decoration.
                let game = match pseudo_branch_label(lang, row.game_id) {
                    Some(label) => Part::decoration(label),
                    None => Part::searched(row.game_name.clone()),
                };
                vec![
                    Part::decoration(open),
                    game,
                    Part::decoration(close),
                    Part::decoration(i18n::FLAT_ROW_SEPARATOR),
                    Part::searched(row.rel_path.clone()),
                ]
            } else {
                vec![Part::searched(row.rel_path.clone())]
            };
            // The anti-cheat verdict is a per-*game* fact (see
            // `ui::tree_view::show_game_row`, which marks it once on the
            // game's own row), not drawn again here - see that function for
            // why.
            (*index, parts)
        }
        _ => unreachable!("Row::File member/node kind mismatch"),
    };
    let display_name = highlight::name(ui, &name_parts, ctx.query);
    let level = indent;

    let item = &mut findings[index];

    let lang_col = match &item.row.lang_tag {
        Some(lang_tag) => egui::RichText::new(format!("[{lang_tag}]")),
        None => egui::RichText::new(""),
    };
    let needs_review = item.row.confidence < REVIEW_CONFIDENCE_THRESHOLD;
    let review_hint = i18n::strings(lang).review_mark_hint;

    // Absolute path (Windows-native separators): the tooltip's first line, the
    // clipboard payload, and the argument to every context-menu shell action.
    let abs_path = row_actions::windows_path_string(&item.row.install_dir.join(&item.row.rel_path));

    let reason = ctx
        .descriptions
        .display(item.row.source, &item.row.rule_desc);
    let mut hover = i18n::hover_reason(lang, &abs_path, &reason, item.row.confidence);
    if let Some(lang_tag) = &item.row.lang_tag {
        hover.push_str(&i18n::hover_lang_suffix(lang, lang_tag));
    }
    if item.row.display_category() == DisplayCategory::Intro {
        hover.push_str(&i18n::hover_stub_suffix(lang));
    }
    // The row shows the on-disk allocated size as primary (allocated-size accounting); when the
    // logical size differs (cluster slack, NTFS compression), spell it out in
    // the tooltip so the two figures are both available without cluttering the
    // row.
    if item.row.size != item.row.size_on_disk {
        hover.push_str(&i18n::hover_logical_size_suffix(
            lang,
            &format_size(lang, item.row.size),
        ));
    }
    // The anti-cheat verdict is explained once, on the game's own row (see
    // `show_game_row`), not repeated per-file here.

    row_columns(
        ui,
        lang_col,
        egui::RichText::new(""),
        egui::RichText::new(format_size(lang, item.row.size_on_disk)),
        |ui| {
            ui.add_space(INDENT_PX * level as f32);
            let is_blocked = !item.row.individually_selectable();
            let checkbox = ui.add_enabled(!is_blocked, egui::Checkbox::new(&mut item.selected, ""));
            if checkbox.changed() {
                ctx.selection_changed.set(true);
            }
            if let Some(reason) = item.row.deletion_block_reason.as_deref() {
                checkbox.on_disabled_hover_text(i18n::deletion_block_reason(lang, reason));
            }
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
                |ui| never_touch_entry(ui, lang, item, ctx.keep_request, index),
            );
        },
    );
}

/// The file row's own context-menu action: "never touch this file in this
/// game", which writes a personal exception (a keep rule scoped to the game -
/// see `gametrimmer_core::rules::RulePolarity`) so the file stops being
/// proposed by every future scan.
///
/// It lives here, in the row's right-click menu, and not in a panel of its
/// own: the decision is about *this* row, it is taken while looking at this
/// row, and the interface has no room for another panel.
///
/// Nothing is written from inside the menu. The click only records the row's
/// index; the write, the row's removal and the status line all happen in
/// [`show`] once the tree's borrows are released, after the row pass has
/// released its disjoint mutable borrow of findings.
///
/// A game with no launcher id cannot be the scope of an exception, so the
/// entry is shown disabled with the reason on hover rather than hidden: an
/// action that silently is not there for some rows is indistinguishable from
/// one that is broken.
fn never_touch_entry(
    ui: &mut egui::Ui,
    lang: Lang,
    item: &FindingItem,
    keep_request: &std::cell::Cell<Option<usize>>,
    index: usize,
) {
    let s = i18n::strings(lang);
    let can_scope = item.row.app_id.is_some();
    let button = ui.add_enabled(can_scope, egui::Button::new(s.ctx_never_touch));
    if !can_scope {
        button.on_disabled_hover_text(s.ctx_never_touch_needs_app_id);
        return;
    }
    if button.clicked() {
        keep_request.set(Some(index));
        ui.close();
    }
}

/// Carries out the "never touch this" click recorded by
/// [`never_touch_entry`]: writes the exception, drops the row from the tree,
/// and says which of the two happened in the status line.
///
/// The row leaves immediately rather than at the next scan. A rule only takes
/// effect when the scanner next runs, but leaving the row sitting in the plan
/// after the user has said "never touch this" would be the app arguing with
/// them; dropping it is the same `removed` flag a deleted file uses, so the
/// database is untouched and the next scan is what makes it stick.
fn apply_keep_request(app: &mut GameTrimmerApp, index: usize) {
    let lang = app.lang();
    let Some(item) = app.findings.get(index) else {
        return;
    };
    let Some(app_id) = item.row.app_id.clone() else {
        return;
    };
    let rel_path = item.row.rel_path.clone();
    // Written per-language, because the pack outlives the interface language
    // it was written under - the same reason rule packs carry a per-language
    // `desc` at all. The game's name is in there so the file can be read and
    // pruned by hand later, which is the only way to undo an exception for
    // now. English is the only entry while localizations are frozen for
    // development (Vikunja #443 tracks unfreezing them).
    let desc = gametrimmer_core::localized::LocalizedText::PerLanguage(
        [(
            "en".to_string(),
            i18n::exception_desc(Lang::En, &item.row.game_name, &rel_path),
        )]
        .into_iter()
        .collect(),
    );

    app.status_message =
        match crate::worker::rules_io::add_personal_exception(lang, &app_id, &rel_path, desc) {
            Ok(message) => {
                if let Some(item) = app.findings.get_mut(index) {
                    item.removed = true;
                    item.selected = false;
                }
                app.tree_dirty = true;
                message
            }
            Err(error) => i18n::error_prefixed(lang, error),
        };
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::ui::harness::UiTest;
    use crate::ui::plan_panel::CLEAR_SEARCH_GLYPH;

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

        open_every_branch(&mut test);
        test.assert_label(SEEDED_FILE_NAME);
        test
    }

    /// Marks every branch, game and category of the *current* tree open, then
    /// settles.
    ///
    /// Keyed off the tree as it stands rather than off the disk axis, because
    /// the collapse keys are namespaced per axis (`model::TopKey`): keys built
    /// under one axis say nothing about another, so a test that switches axes
    /// has to open the new tree rather than reuse the old one's keys.
    fn open_every_branch(test: &mut UiTest) {
        let mut keys = Vec::new();
        for top_group in &test.app().tree {
            keys.push(top_key(&top_group.key));
            for game in &top_group.games {
                keys.push(game_key(&top_group.key, game.game_id));
                for category_node in &game.categories {
                    keys.push(category_key(
                        &top_group.key,
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
    }

    /// Opens every folder node as well - `open_every_branch` stops at the
    /// category level, and a folder's member rows only exist once it is open.
    fn open_every_folder(test: &mut UiTest) {
        let mut keys = Vec::new();
        for top_group in &test.app().tree {
            for game in &top_group.games {
                for category_node in &game.categories {
                    for node in &category_node.nodes {
                        if let model::TreeNode::Folder { group_dir, .. } = node {
                            keys.push(folder_key(
                                &top_group.key,
                                game.game_id,
                                category_node.category,
                                group_dir,
                            ));
                        }
                    }
                }
            }
        }
        for key in keys {
            test.app_mut().tree_toggles.insert(key, true);
        }
        test.run();
    }

    /// Switches axis and opens whatever the new tree turned out to be.
    fn regroup(test: &mut UiTest, axis: model::GroupAxis) {
        test.app_mut().set_tree_axis(axis);
        test.run();
        open_every_branch(test);
    }

    /// The whole label a file row carries under the flat axis, which is the
    /// only axis whose rows name their own game. Assembled from the same two
    /// i18n pieces `show_file_row` assembles it from, so the pieces are what
    /// the assertions are pinned to rather than a second copy of the text.
    fn flat_row_name(lang: Lang, game: &str, rel_path: &str) -> String {
        format!(
            "{}{}{rel_path}",
            i18n::quoted(lang, game),
            i18n::FLAT_ROW_SEPARATOR,
        )
    }

    /// GT-38: a game no launcher lists is marked, and one that a launcher
    /// lists is not. Both halves matter - a marker on every row says nothing.
    #[test]
    fn a_game_no_launcher_lists_is_marked_in_the_tree() {
        let mut test = UiTest::new(show);
        test.seed_findings();
        // The seeded games carry a launcher id; take it away from the first
        // only, so the two rows differ in exactly the thing under test.
        test.app_mut().findings[0].row.app_id = None;
        test.app_mut().rebuild_tree();
        test.run();

        let lang = test.app().lang();
        let marked = format!("{} ◇", i18n::quoted(lang, "Test Game 0"));
        test.assert_label(&marked);

        // The launcher-known one keeps its plain quoted name.
        test.assert_label(&i18n::quoted(lang, "Test Game 1"));
        test.assert_no_label(&format!("{} ◇", i18n::quoted(lang, "Test Game 1")));
    }

    /// The two seeded games, in the order `UiTest::seed_findings` creates
    /// them - which is also alphabetical, so a reversal is visible.
    const SEEDED_GAMES: [&str; 2] = ["Test Game 0", "Test Game 1"];

    /// Where a seeded game's row currently sits, by the label the tree draws
    /// for it (game names are rendered quoted, see `show_game_row`).
    fn game_row_top(test: &UiTest, game: &str) -> f32 {
        test.rect_of(&i18n::quoted(test.app().lang(), game)).top()
    }

    /// A heading as the row draws it - the column's name plus the glyph for
    /// the order it is in (see `header_cell`).
    fn heading(title: &str, glyph: &str) -> String {
        format!("{title} {glyph}")
    }

    /// A sortable heading has to say so while it is still untouched. A control
    /// that only reveals itself under the pointer is one nobody knows to look
    /// for - and the language heading, which is not a control, must not wear
    /// the mark and invite a click that does nothing.
    #[test]
    fn every_sortable_heading_is_marked_before_it_is_touched() {
        let mut test = tree_of_files([90; 2]);
        let s = test.strings();

        for title in [s.col_name, s.col_files, s.col_size] {
            test.assert_label(&heading(title, SORT_AVAILABLE_GLYPH));
        }

        test.assert_label(s.col_language);
        test.assert_no_label(&heading(s.col_language, SORT_AVAILABLE_GLYPH));

        test.click(s.col_language);
        assert_eq!(
            test.app().tree_sort,
            None,
            "the language heading is a label, not a control - clicking it must order nothing",
        );
    }

    /// The mark costs every heading a glyph's worth of width, and the fixed
    /// columns are narrow. It must not push a heading out of its own cell and
    /// over its neighbour, which is what would knock the whole table's headings
    /// out of line with the rows below them.
    #[test]
    fn a_marked_heading_stays_inside_its_column() {
        let test = tree_of_files([90; 2]);
        let s = test.strings();

        let files = test.rect_of(&heading(s.col_files, SORT_AVAILABLE_GLYPH));
        let size = test.rect_of(&heading(s.col_size, SORT_AVAILABLE_GLYPH));

        assert!(
            files.width() <= COUNT_COLUMN_PX,
            "the \"{}\" heading is {} wide in a {COUNT_COLUMN_PX}pt column",
            s.col_files,
            files.width(),
        );
        assert!(
            size.width() <= SIZE_COLUMN_PX,
            "the \"{}\" heading is {} wide in a {SIZE_COLUMN_PX}pt column",
            s.col_size,
            size.width(),
        );
        assert!(
            files.right() <= size.left(),
            "the two right-hand headings overlap: {files:?} runs into {size:?}",
        );
    }

    /// Each heading orders the tree by its column, reverses on the next click,
    /// and hands the tree's own order back on the third (GT-31).
    #[test]
    fn clicking_a_column_heading_cycles_it_through_both_directions_and_back() {
        let mut test = tree_of_files([90; 2]);
        let size = test.strings().col_size;

        assert_eq!(
            test.app().tree_sort,
            None,
            "a freshly built tree is in its own order, with no column claiming it",
        );

        test.click(&heading(size, SORT_AVAILABLE_GLYPH));
        assert_eq!(
            test.app().tree_sort,
            Some(TreeSort {
                column: SortColumn::Size,
                descending: true,
            }),
        );
        test.assert_label(&heading(size, SORT_DESCENDING_GLYPH));

        test.click(&heading(size, SORT_DESCENDING_GLYPH));
        assert_eq!(
            test.app().tree_sort,
            Some(TreeSort {
                column: SortColumn::Size,
                descending: false,
            }),
        );
        test.assert_label(&heading(size, SORT_ASCENDING_GLYPH));

        test.click(&heading(size, SORT_ASCENDING_GLYPH));
        assert_eq!(
            test.app().tree_sort,
            None,
            "the third click has to return the tree's designed order, not a fourth sort",
        );
        test.assert_label(&heading(size, SORT_AVAILABLE_GLYPH));
    }

    /// A column opens in the direction its own content is asked about: largest
    /// first for the numeric columns, A first for the textual one. Also covers
    /// moving the sort from one column to another.
    #[test]
    fn a_numeric_heading_opens_at_the_largest_and_a_textual_one_at_a() {
        let mut test = tree_of_files([90; 2]);
        let s = test.strings();

        test.click(&heading(s.col_name, SORT_AVAILABLE_GLYPH));
        assert_eq!(
            test.app().tree_sort,
            Some(TreeSort {
                column: SortColumn::Name,
                descending: false,
            }),
        );

        test.click(&heading(s.col_files, SORT_AVAILABLE_GLYPH));
        assert_eq!(
            test.app().tree_sort,
            Some(TreeSort {
                column: SortColumn::Files,
                descending: true,
            }),
            "moving the sort to another column starts that column's own way round",
        );
    }

    /// The click has to reach the rows, not only the state behind them and the
    /// arrow in the heading.
    ///
    /// The seeded games are equal in size, so their default order is already
    /// alphabetical - the first assertion is the baseline, and the reversal
    /// after it is the claim.
    #[test]
    fn reversing_a_heading_moves_the_rows_on_screen() {
        let mut test = tree_of_files([90; 2]);
        let name = test.strings().col_name;

        test.click(&heading(name, SORT_AVAILABLE_GLYPH));
        assert!(
            game_row_top(&test, SEEDED_GAMES[0]) < game_row_top(&test, SEEDED_GAMES[1]),
            "ascending by name puts {} above {}",
            SEEDED_GAMES[0],
            SEEDED_GAMES[1],
        );

        test.click(&heading(name, SORT_ASCENDING_GLYPH));
        assert!(
            game_row_top(&test, SEEDED_GAMES[1]) < game_row_top(&test, SEEDED_GAMES[0]),
            "reversing the order has to move the rows, not just flip the arrow",
        );
    }

    /// A sort is a preference about how to read a tree, not a selection keyed
    /// to one result set - so a delete, which rebuilds the tree from scratch,
    /// must not quietly drop it.
    #[test]
    fn a_chosen_order_survives_a_tree_rebuild() {
        let mut test = tree_of_files([90; 2]);
        test.click(&heading(test.strings().col_name, SORT_AVAILABLE_GLYPH));
        let chosen = test.app().tree_sort;

        test.app_mut().rebuild_tree();
        test.run();

        assert_eq!(test.app().tree_sort, chosen);
        assert!(
            game_row_top(&test, SEEDED_GAMES[0]) < game_row_top(&test, SEEDED_GAMES[1]),
            "the rebuilt tree fell back to the default order",
        );
    }

    /// A search that matches nothing must say so. Before this, the empty tree
    /// fell through to the same "press Scan libraries" hint an unscanned
    /// window shows - advice that is simply wrong when the scan did find
    /// things and the query is what hid them (MT-F05).
    #[test]
    fn a_search_that_matches_nothing_says_so_instead_of_offering_a_scan() {
        let mut test = tree_of_files([90; 2]);
        let s = test.strings();

        test.app_mut()
            .set_search_query("no-such-file-anywhere".to_string());
        test.run();

        test.assert_label(s.search_no_matches);
        test.assert_no_label(s.no_findings_hint);
    }

    /// Tinting the match rewrites the name as a multi-section layout job, and
    /// a job that dropped, duplicated or reordered a piece would still be a
    /// perfectly valid job. The label is the one thing that says it did not:
    /// it is the concatenation of every section, so it has to come back
    /// character for character what it was before the search was typed.
    ///
    /// This is also the whole of what the harness can see - the tint itself is
    /// a background colour on a section, which the accessibility tree does not
    /// carry, so it is pinned by `ui::highlight`'s own tests instead.
    #[test]
    fn tinting_a_match_leaves_the_row_names_exactly_as_they_read() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        let game = i18n::quoted(lang, SEEDED_GAMES[0]);

        // Matches inside a file name and inside a game name, one at a time, so
        // each row kind is drawn with a tint of its own rather than riding on
        // a query that only lit up its neighbour.
        for query in ["loc_0", "test game 0"] {
            test.app_mut().set_search_query(query.to_string());
            test.run();

            test.assert_label(SEEDED_FILE_NAME);
            test.assert_label(&game);
            assert_eq!(
                test.count_labels(SEEDED_FILE_NAME),
                1,
                "the tinted name for {query:?} was drawn as more than one label",
            );
        }
    }

    /// The flat row's name is four pieces - quotation marks, the game, a dash,
    /// the path - and only two of them are the search's to tint. It still has
    /// to read as one name.
    #[test]
    fn a_tinted_flat_row_still_reads_as_one_name() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        regroup(&mut test, model::GroupAxis::Flat);
        let name = flat_row_name(lang, SEEDED_GAMES[0], "data/loc_0.pak");

        for query in ["loc_0", "test game 0"] {
            test.app_mut().set_search_query(query.to_string());
            test.run();
            test.assert_label(&name);
        }
    }

    /// The same window with no query at all keeps the original hint: the two
    /// empty states must not collapse into one message.
    #[test]
    fn an_unsearched_empty_tree_still_offers_the_scan() {
        let mut test = UiTest::new(show);
        test.app_mut().accept_disclaimer();
        test.app_mut().mark_scan_started();
        test.run();
        let s = test.strings();

        test.assert_label(s.no_findings_hint);
        test.assert_no_label(s.search_no_matches);
    }

    /// The button that empties the field, offered only while there is
    /// something in it (MT-F05).
    #[test]
    fn the_search_field_offers_a_way_to_clear_itself() {
        let mut test = tree_of_files([90; 2]);

        // Nothing typed: no button to find.
        test.assert_no_label(CLEAR_SEARCH_GLYPH);

        test.app_mut().set_search_query("loc_0".to_string());
        test.run();
        test.assert_label(CLEAR_SEARCH_GLYPH);

        test.click(CLEAR_SEARCH_GLYPH);
        assert!(
            test.app().tree_search.is_empty(),
            "the clear button left the query in place: {:?}",
            test.app().tree_search,
        );
        test.assert_label(SEEDED_FILE_NAME);
    }

    /// Typing the first character makes the conditional clear button appear.
    /// That must not change the text edit's identity and strand keyboard focus
    /// on a widget which no longer exists on the next frame.
    #[test]
    fn typing_keeps_the_search_field_focused_when_the_clear_button_appears() {
        let mut test = tree_of_files([90; 2]);

        test.focus_only_role(egui::accesskit::Role::TextInput);
        test.type_text("l");
        assert_eq!(test.app().tree_search, "l");

        test.type_text("o");
        assert_eq!(
            test.app().tree_search,
            "lo",
            "the second keystroke was lost after the clear button appeared",
        );
    }

    /// Clicking a row's empty width folds it, so the widened click target does
    /// something visible beyond moving the cursor (MT-E07).
    #[test]
    fn a_click_on_a_rows_empty_width_collapses_it() {
        let mut test = tree_of_files([90; 2]);

        // The disk row is the topmost one and starts open (see `tree_of_files`).
        let row = test.rect_of(SEEDED_FILE_NAME);
        let disk_label = i18n::disk_label(test.app().lang(), "C:");
        let disk_row_y = test.rect_of(&disk_label).center().y;
        // Far right of the row, past every widget it draws - the inert width
        // this test is about.
        let empty_spot = egui::pos2(row.right() - 4.0, disk_row_y);

        test.click_at(empty_spot);
        test.assert_no_label(SEEDED_FILE_NAME);

        // And the same click again brings it back - folding is not one-way.
        test.click_at(empty_spot);
        test.assert_label(SEEDED_FILE_NAME);
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
        for confidence in [REVIEW_CONFIDENCE_THRESHOLD - 1, REVIEW_CONFIDENCE_THRESHOLD] {
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
        let mut test =
            tree_of_files([REVIEW_CONFIDENCE_THRESHOLD - 1, REVIEW_CONFIDENCE_THRESHOLD]);
        assert_eq!(test.count_labels("\u{26a0}"), 1, "one row is below the bar");

        test.hover("\u{26a0}");

        test.assert_label(test.strings().review_mark_hint);
    }

    /// The control: a confident finding gets no mark. Without this the
    /// assertion above would pass just as well if every row were marked,
    /// which would make the mark furniture.
    #[test]
    fn a_confident_finding_carries_no_mark() {
        let test = tree_of_files([REVIEW_CONFIDENCE_THRESHOLD; 2]);

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
        let marked = tree_of_files([REVIEW_CONFIDENCE_THRESHOLD - 1; 2])
            .rect_of(SEEDED_FILE_NAME)
            .min
            .x;
        let unmarked = tree_of_files([REVIEW_CONFIDENCE_THRESHOLD; 2])
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

    /// The bug: the tree's own modal list named three of the
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

    /// The keyboard toggle mutates `findings` from `handle_keyboard`, nowhere
    /// near the checkbox code, so it is the selection path most likely to be
    /// missed when the others are changed. It used to be tested through the
    /// selection profile it dropped to `Custom`; GT-89 removed profiles, and
    /// what is left to assert is the thing that always mattered - the row the
    /// cursor is on actually changes state, and changes back.
    #[test]
    fn the_keyboard_toggle_ticks_the_row_under_the_cursor() {
        let mut test = tree_with_cursor();
        test.run();
        let before: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();

        test.press(egui::Key::Space);
        let after: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();
        assert_ne!(
            after, before,
            "Space on a cursor row left every checkbox where it was",
        );

        test.press(egui::Key::Space);
        assert_eq!(
            test.app()
                .findings
                .iter()
                .map(|f| f.selected)
                .collect::<Vec<bool>>(),
            before,
            "a second Space must put the row back rather than latch it",
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
    /// right-aligned columns - the dead width whole-row interaction is about.
    fn empty_space_right_of(name_rect: egui::Rect) -> egui::Pos2 {
        egui::pos2(name_rect.max.x + 24.0, name_rect.center().y)
    }

    /// whole-row interaction: the row, not just its name, is the click target.
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

    /// The safety half of whole-row interaction: a wide, easily-hit target moves the cursor
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

    // -- grouping axes (GT-35) --

    /// The seeded games' launchers, as their branch headings read on screen.
    fn seeded_launcher_labels(test: &UiTest) -> Vec<String> {
        let lang = test.app().lang();
        crate::ui::harness::SEEDED_LIBRARIES
            .iter()
            .map(|(vendor, _)| i18n::launcher_label(lang, vendor))
            .collect()
    }

    /// The whole point of the axis: the same findings, cut a different way,
    /// with no rescan. Both seeded games share disk C: and come from different
    /// launchers, so the headings change and the file rows do not.
    #[test]
    fn switching_to_the_launcher_axis_replaces_the_disk_heading() {
        let mut test = tree_of_files([90; 2]);
        let disk = i18n::disk_label(test.app().lang(), "C:");
        test.assert_label(&disk);

        test.app_mut().set_tree_axis(model::GroupAxis::Launcher);
        test.run();

        test.assert_no_label(&disk);
        for launcher in seeded_launcher_labels(&test) {
            test.assert_label(&launcher);
        }
    }

    /// A switch must never be able to lose a finding - that is what would make
    /// the control dangerous rather than merely useless. Asserted on the model
    /// the tree is built from, across every axis, because a row hidden inside
    /// a collapsed branch is legitimately off screen while still present.
    #[test]
    fn every_axis_still_holds_every_finding() {
        let mut test = tree_of_files([90; 2]);
        let expected = test.app().findings.len();

        for axis in model::GROUP_AXIS_ORDER {
            test.app_mut().set_tree_axis(axis);
            test.run();
            let reachable: usize = test
                .app()
                .tree
                .iter()
                .map(|group| group.all_indices.len())
                .sum();
            assert_eq!(reachable, expected, "grouping by {axis:?} lost a finding");
        }
    }

    /// GT-35's second pitfall, from the user's side: the collapse keys are
    /// namespaced per axis, so a branch opened under one axis is still open
    /// after a round trip through another - rather than the tree folding shut
    /// because a key from the other axis answered for it.
    #[test]
    fn expanding_a_branch_survives_a_round_trip_through_another_axis() {
        let mut test = tree_of_files([90; 2]);
        test.assert_label(SEEDED_FILE_NAME);

        test.app_mut().set_tree_axis(model::GroupAxis::Launcher);
        test.run();
        test.app_mut().set_tree_axis(model::GroupAxis::Disk);
        test.run();

        test.assert_label(SEEDED_FILE_NAME);
    }

    /// Switching axes is a view change, not an edit. If it could touch the
    /// checkboxes it would change what a pending delete is about to remove.
    #[test]
    fn switching_axes_leaves_the_selection_alone() {
        let mut test = tree_of_files([90; 2]);
        let before: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();

        test.app_mut().set_tree_axis(model::GroupAxis::Library);
        test.run();

        let after: Vec<bool> = test.app().findings.iter().map(|f| f.selected).collect();
        assert_eq!(before, after);
    }

    /// The category axis heads the tree with the category and drops the
    /// per-game category row. Both halves are asserted: the heading must be
    /// there once, not twice, or the fold has bought nothing.
    #[test]
    fn the_category_axis_states_the_category_once() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        let loc = category_display(lang, DisplayCategory::Loc);
        assert_eq!(
            test.count_labels(loc),
            2,
            "the disk axis draws the seeded games' category row once per game",
        );

        regroup(&mut test, model::GroupAxis::Category);

        assert_eq!(
            test.count_labels(loc),
            1,
            "the branch heading and a category row under each game say it twice",
        );
        test.assert_no_label(&i18n::disk_label(lang, "C:"));
        // The games are still there - it is the redundant heading that went,
        // not the level that says whose file this is.
        for game in SEEDED_GAMES {
            test.assert_label(&i18n::quoted(lang, game));
        }
    }

    /// The orphan pseudo-game is the one game row that says nothing new under
    /// the category axis: its branch heading is already "Orphaned". Two rows
    /// running "Orphaned / Orphaned residue" is the redundancy that folds the
    /// category level away in the first place.
    #[test]
    fn the_orphan_pseudo_game_folds_away_under_the_category_axis() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        let branch_label = test.strings().orphan_branch_label;

        let mut orphan = test.app().findings[0].clone();
        orphan.row.file_id = 99;
        orphan.row.game_id = model::ORPHAN_GAME_ID;
        orphan.row.game_name = String::new();
        orphan.row.install_dir = std::path::PathBuf::from("C:\\SteamLibrary\\steamapps");
        orphan.row.rel_path = "downloading".to_string();
        orphan.row.source =
            model::FindingSource::Orphan(gametrimmer_core::orphans::OrphanKind::ServiceFolder);
        orphan.row.lang_tag = None;
        test.app_mut().findings.push(orphan);
        test.app_mut().rebuild_tree();
        test.app_mut().clear_search();
        open_every_branch(&mut test);

        // Under the disk axis the pseudo-game is the row that gives the
        // residue a home, so it is drawn.
        test.assert_label(branch_label);

        regroup(&mut test, model::GroupAxis::Category);

        test.assert_label(category_display(lang, DisplayCategory::Orphan));
        test.assert_no_label(branch_label);
        test.assert_label("downloading");
    }

    /// A janitor artifact - a crash dump, a shader cache, a launcher cache, a
    /// save - is not launcher residue, and must not be filed under the heading
    /// that says it is. It gets its own branch, and keeps its own category.
    #[test]
    fn a_janitor_finding_sits_under_the_system_branch_and_not_the_orphan_one() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        let orphan_label = test.strings().orphan_branch_label;
        let system_label = test.strings().system_branch_label;

        let mut dump = test.app().findings[0].clone();
        dump.row.file_id = 98;
        dump.row.game_id = model::SYSTEM_GAME_ID;
        dump.row.game_name = String::new();
        dump.row.install_dir = std::path::PathBuf::from(r"C:\Users\Test\AppData\Local\CrashDumps");
        dump.row.rel_path = "game.exe.4242.dmp".to_string();
        dump.row.source = model::FindingSource::Rule(gametrimmer_core::rules::Category::CrashDump);
        dump.row.lang_tag = None;
        test.app_mut().findings.push(dump);
        test.app_mut().rebuild_tree();
        test.app_mut().clear_search();
        open_every_branch(&mut test);

        test.assert_label(system_label);
        test.assert_no_label(orphan_label);
        test.assert_label("game.exe.4242.dmp");

        regroup(&mut test, model::GroupAxis::Category);

        // Under the category axis it keeps its own heading - unlike the orphan
        // branch, "Crashes / System and launcher files" says two different
        // things, so neither row is redundant.
        test.assert_label(category_display(lang, DisplayCategory::Crashes));
        test.assert_label(system_label);
    }

    /// A save area is hundreds of files across dozens of games. Listing them
    /// loose under one heading is a list nobody can answer "which of these do
    /// I want gone" from - each one has to sit under the game that wrote it,
    /// which is what the row's `group_dir` is for.
    #[test]
    fn saves_are_drawn_under_the_game_folder_that_holds_them() {
        let mut test = tree_of_files([90; 2]);
        let system_label = test.strings().system_branch_label;

        for (file_id, name) in [(101, "autosave1.ess"), (102, "autosave2.ess")] {
            let mut save = test.app().findings[0].clone();
            save.row.file_id = file_id;
            save.row.game_id = model::SYSTEM_GAME_ID;
            save.row.game_name = String::new();
            save.row.install_dir = std::path::PathBuf::from(r"E:\Documents\My Games");
            save.row.rel_path = format!(r"Skyrim Special Edition\Saves\{name}");
            save.row.group_dir = Some("Skyrim Special Edition".to_string());
            save.row.source =
                model::FindingSource::Rule(gametrimmer_core::rules::Category::SaveBloat);
            save.row.lang_tag = None;
            test.app_mut().findings.push(save);
        }
        test.app_mut().rebuild_tree();
        test.app_mut().clear_search();
        open_every_branch(&mut test);
        open_every_folder(&mut test);

        test.assert_label(system_label);
        // The game's folder is a heading of its own...
        test.assert_label(r"Skyrim Special Edition\");
        // ...and the rows below it drop that prefix rather than repeating it.
        test.assert_label(r"Saves\autosave1.ess");
        test.assert_no_label(r"Skyrim Special Edition\Saves\autosave1.ess");
    }

    /// The flat axis draws no headings at all, and its file rows have to carry
    /// the game themselves - a list of bare "loc_0.pak" would not say which
    /// game each came from.
    #[test]
    fn the_flat_axis_draws_only_file_rows_that_name_their_game() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();

        regroup(&mut test, model::GroupAxis::Flat);

        test.assert_no_label(&i18n::disk_label(lang, "C:"));
        for game in SEEDED_GAMES {
            test.assert_no_label(&i18n::quoted(lang, game));
        }
        test.assert_no_label(category_display(lang, DisplayCategory::Loc));
        test.assert_no_label(SEEDED_FILE_NAME);

        for (i, game) in SEEDED_GAMES.iter().enumerate() {
            test.assert_label(&flat_row_name(lang, game, &format!("data/loc_{i}.pak")));
        }
    }

    /// Folding a level away has to move the rows under it, not just hide the
    /// heading - an indent that stayed put would leave the tree claiming a
    /// parent that is no longer on screen.
    #[test]
    fn folding_a_level_away_pulls_its_rows_back_one_indent() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        let by_disk = test.rect_of(SEEDED_FILE_NAME).left();

        regroup(&mut test, model::GroupAxis::Category);

        let by_category = test.rect_of(SEEDED_FILE_NAME).left();
        assert!(
            (by_disk - by_category - INDENT_PX).abs() < 1.0,
            "dropping the category row should pull the file one indent left: \
             {by_disk} -> {by_category}, indent is {INDENT_PX}",
        );

        regroup(&mut test, model::GroupAxis::Flat);

        let flat = test
            .rect_of(&flat_row_name(lang, SEEDED_GAMES[0], "data/loc_0.pak"))
            .left();
        assert!(
            flat < by_category,
            "the flat axis folds every heading away, so its rows sit at the margin: \
             {flat} is not left of {by_category}",
        );
    }

    /// Under the flat axis no heading is left to answer the category filter, so
    /// each file answers for itself. Without that the filter would either hide
    /// everything or hide nothing - and this is the axis where "show me only
    /// the biggest localizations" is the obvious thing to ask.
    #[test]
    fn the_category_filter_still_works_under_the_flat_axis() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        regroup(&mut test, model::GroupAxis::Flat);
        let first = flat_row_name(lang, SEEDED_GAMES[0], "data/loc_0.pak");
        test.assert_label(&first);

        // The seeded findings are all localizations, so filtering to a
        // category they are not empties the list, and filtering to their own
        // leaves it whole.
        test.app_mut()
            .set_category_filter(Some(DisplayCategory::Redist));
        test.run();
        test.assert_no_label(&first);
        test.assert_label(test.strings().search_no_matches);

        test.app_mut()
            .set_category_filter(Some(DisplayCategory::Loc));
        test.run();
        test.assert_label(&first);
    }

    /// `DisplayCategory::Intro` renders under its category header, filters
    /// correctly with the category selector, and appends the micro-stub
    /// explanation to its file row hover tooltip.
    #[test]
    fn intro_category_renders_tree_node_category_filter_and_hover_stub_text() {
        let mut test = tree_of_files([90; 2]);
        let lang = test.app().lang();
        let intro_heading = category_display(lang, DisplayCategory::Intro);

        let mut intro_finding = test.app().findings[0].clone();
        intro_finding.row.file_id = 42;
        intro_finding.row.rel_path = "movies/intro_logo.bik".to_string();
        intro_finding.row.source =
            model::FindingSource::Rule(gametrimmer_core::rules::Category::Intro);
        intro_finding.row.rule_desc = "Intro splash video".to_string();
        intro_finding.row.lang_tag = None;
        test.app_mut().findings.push(intro_finding);
        test.app_mut().rebuild_tree();
        test.app_mut().clear_search();
        open_every_branch(&mut test);

        test.assert_label(intro_heading);
        test.assert_label("movies/intro_logo.bik");

        test.hover("movies/intro_logo.bik");
        test.assert_label_containing(test.strings().hover_stub_note);

        test.app_mut()
            .set_category_filter(Some(DisplayCategory::Intro));
        test.run();
        test.assert_label("movies/intro_logo.bik");
        test.assert_no_label(SEEDED_FILE_NAME);

        test.app_mut()
            .set_category_filter(Some(DisplayCategory::Loc));
        test.run();
        test.assert_label(SEEDED_FILE_NAME);
        test.assert_no_label("movies/intro_logo.bik");
    }

    /// The owner's decision after testing on their real library: intro is no
    /// longer treated as risky. A protected row that rewrites bytes in place
    /// without being a monolithic archive - an intro finding, replaced by a
    /// micro-stub rather than deleted - is exactly as ordinary as a
    /// whole-file delete for anti-cheat purposes, so it is swept up by
    /// Select All like any other row and the resulting batch clears the
    /// preflight. Before this change, excluding it hid 56 intro findings in
    /// Assassin's Creed Shadows (anti-cheat protected) from Select All and
    /// every group header, with no visible reason.
    #[test]
    fn select_all_sweeps_an_anti_cheat_protected_intro_row() {
        let mut test = tree_of_files([90, 90]);
        {
            let app = test.app_mut();
            for item in &mut app.findings {
                item.selected = false;
            }
            app.findings[0].row.anti_cheat_protected = true;
            app.findings[0].row.source =
                model::FindingSource::Rule(gametrimmer_core::rules::Category::Intro);
            app.rebuild_tree();
        }
        open_every_branch(&mut test);

        test.app_mut().select_all();

        assert!(
            test.app().findings[0].selected,
            "an anti-cheat protected intro row must be swept up by Select All like any other \
             whole-file delete"
        );
        assert!(
            test.app().findings[1].selected,
            "a plain row must still be caught by Select All"
        );

        let checked = crate::deletion_controller::validate_batch(&test.app().findings, &[0, 1]);
        assert!(
            checked.is_ok(),
            "the swept-up anti-cheat intro row must pass the batch preflight"
        );
    }

    /// The narrowed half of the carve-out, and what the fix actually
    /// restores: a localization row - a whole-file delete, never rewritten in
    /// place - stays bulk-selectable in a protected game and is swept up by
    /// Select All exactly like an unprotected row. The old blanket rule
    /// (`anti_cheat_protected` alone disqualifying a row) took Select All away
    /// from every finding in every protected game, since the verdict is
    /// per-game - 112k+ findings across 162 games on the reported library.
    #[test]
    fn select_all_sweeps_an_anti_cheat_protected_loc_row() {
        let mut test = tree_of_files([90, 90]);
        for item in &mut test.app_mut().findings {
            item.selected = false;
        }
        test.app_mut().findings[0].row.anti_cheat_protected = true;

        test.app_mut().select_all();

        assert!(
            test.app().findings[0].selected,
            "a localization row in a protected game must be swept up by Select All like any \
             other whole-file delete"
        );
        assert!(test.app().findings[1].selected);
    }

    /// A game where every row is still `imported_untrusted` (an older
    /// database's evidence this scan never re-checked) has zero
    /// bulk-selectable rows in its group, while every row remains
    /// individually selectable by hand. The old header math counted only
    /// bulk-selectable rows on both sides of the
    /// fraction, so a hand tick here was invisible to it and the header kept
    /// reporting "nothing selected" - `(false, false)` - while a file really
    /// was queued for deletion. This is the game-level header
    /// (`show_header_row` backs every level - disk, game, category, folder -
    /// through the same `group_selection_state` call), verified directly
    /// against the tree the harness actually built.
    #[test]
    fn group_header_reports_selection_honestly_when_every_row_is_imported_untrusted() {
        let mut test = UiTest::new(show);
        test.seed_many_findings(1, 2);
        {
            let app = test.app_mut();
            for item in &mut app.findings {
                item.selected = false;
                item.row.imported_untrusted = true;
            }
            app.rebuild_tree();
        }
        open_every_branch(&mut test);

        let indices = test.app().tree[0].games[0].all_indices.clone();
        assert_eq!(indices.len(), 2, "both seeded files land in the one game");

        assert_eq!(
            group_selection_state(&test.app().findings, &indices),
            (false, false),
            "untouched, nothing is selected yet"
        );

        test.app_mut().findings[indices[0]].selected = true;
        assert_eq!(
            group_selection_state(&test.app().findings, &indices),
            (false, true),
            "a hand tick must be visible - not complete, but never 'nothing selected'"
        );

        test.app_mut().findings[indices[1]].selected = true;
        assert_eq!(
            group_selection_state(&test.app().findings, &indices),
            (true, true),
            "once every row that can ever be selected has been hand-ticked, the group reads complete"
        );
    }

    /// Companion to the honesty test above: the header checkbox itself must
    /// never be a dead click. With something selected, clicking it must
    /// clear the group; with nothing selected and nothing bulk-selectable to
    /// grab, there is nothing a click could do, and the rendered checkbox
    /// must actually be disabled - not merely inert underneath a control that
    /// still looks clickable, which calling `toggle_group` directly (bypassing
    /// `show_header_row`'s `add_enabled`/`on_disabled_hover_text` entirely)
    /// would never catch.
    #[test]
    fn group_header_click_clears_a_selection_or_is_disabled_when_nothing_can_be_selected() {
        let mut test = UiTest::new(show);
        test.seed_many_findings(1, 2);
        {
            let app = test.app_mut();
            for item in &mut app.findings {
                item.selected = false;
                item.row.imported_untrusted = true;
            }
            app.rebuild_tree();
        }
        open_every_branch(&mut test);

        let indices = test.app().tree[0].games[0].all_indices.clone();
        let has_bulk_selectable = indices
            .iter()
            .any(|&i| test.app().findings[i].row.bulk_selectable());
        assert!(
            !has_bulk_selectable,
            "the whole group is still imported_untrusted, so nothing is bulk-selectable"
        );

        // Nothing selected, nothing bulk-selectable: the rendered header
        // checkbox (index 1 - index 0 is the disk root) must be disabled, a
        // click on it must be a true no-op, and it must say why.
        let checkbox = test.nth_checkbox_rect(1);
        test.click_at(checkbox.center());
        assert!(
            test.app().findings.iter().all(|item| !item.selected),
            "a disabled header checkbox must not react to a click"
        );
        test.hover_nth_checkbox(1);
        test.assert_label_containing(test.strings().group_checkbox_disabled_hint);

        // Hand-tick one row - the header becomes clickable again, and this
        // time the click must clear it rather than retry a dead select.
        test.app_mut().findings[indices[0]].selected = true;
        test.run();
        let checkbox = test.nth_checkbox_rect(1);
        test.click_at(checkbox.center());
        assert!(
            test.app().findings.iter().all(|item| !item.selected),
            "the now-enabled header must clear the hand-ticked row on click"
        );
    }

    /// Reported from a real library (originally against a game whose intro
    /// category was wholly anti-cheat protected, before the carve-out
    /// narrowed to monolithic archives only): clicking the group's checkbox
    /// collapsed the group instead of selecting it. Kept alive on a
    /// still-disabled group - now `imported_untrusted`, since that is the
    /// only way left to get a fully non-bulk-selectable group - because the
    /// underlying bug is about a disabled checkbox in general, not about
    /// anti-cheat specifically.
    ///
    /// A disabled widget senses nothing, so the whole-row click target
    /// underneath took the click and folded the row - the one thing a click
    /// on a *checkbox* must never do. The header's other test above only
    /// checked that nothing got selected, which stayed true the whole time
    /// the bug was live, which is why it did not catch this.
    #[test]
    fn clicking_a_disabled_group_checkbox_does_not_fold_the_group() {
        let mut test = UiTest::new(show);
        test.seed_many_findings(1, 2);
        {
            let app = test.app_mut();
            for item in &mut app.findings {
                item.selected = false;
                item.row.imported_untrusted = true;
            }
            app.rebuild_tree();
        }
        open_every_branch(&mut test);

        let before = test.checkbox_count();
        assert!(
            before > 2,
            "the fixture must have children on screen for a fold to be visible: {before}"
        );

        // The game header's checkbox (index 1; index 0 is the disk root).
        let checkbox = test.nth_checkbox_rect(1);
        test.click_at(checkbox.center());

        assert_eq!(
            test.checkbox_count(),
            before,
            "clicking the disabled header checkbox must leave the group open - a click \
             aimed at a checkbox must never fold the tree under the pointer"
        );
        assert!(
            test.app().findings.iter().all(|item| !item.selected),
            "and it must still select nothing"
        );
    }

    /// This is what the narrowed carve-out actually restores: a game that is
    /// entirely anti-cheat protected but holds only ordinary (whole-file
    /// delete) findings behaves like any unprotected group - the header
    /// checkbox is enabled from the start and a real click on it, driven
    /// through the harness rather than called on the model directly, selects
    /// every row. Before this fix `anti_cheat_protected` alone disqualified a
    /// row from bulk selection, so this exact scenario - the common one, since
    /// the verdict is per-game - left the header permanently disabled.
    #[test]
    fn group_header_selects_all_ordinary_rows_in_a_fully_anti_cheat_protected_game() {
        let mut test = UiTest::new(show);
        test.seed_many_findings(1, 2);
        for item in &mut test.app_mut().findings {
            item.selected = false;
            item.row.anti_cheat_protected = true;
        }
        open_every_branch(&mut test);

        let indices = test.app().tree[0].games[0].all_indices.clone();
        assert_eq!(indices.len(), 2);
        assert!(
            indices
                .iter()
                .all(|&i| test.app().findings[i].row.bulk_selectable()),
            "a whole-file delete in a protected game must stay bulk-selectable"
        );

        let checkbox = test.nth_checkbox_rect(1);
        test.click_at(checkbox.center());
        assert!(
            test.app().findings.iter().all(|item| item.selected),
            "the header must select every ordinary row in one click, exactly like an \
             unprotected group"
        );

        test.click_at(checkbox.center());
        assert!(
            test.app().findings.iter().all(|item| !item.selected),
            "clicking again must clear the selection it just made"
        );
    }

    /// The game-level marking Change 2 adds: the anti-cheat verdict is a
    /// per-*game* fact (`FindingRow::anti_cheat_protected`, uniform across
    /// every one of a game's findings). It is marked once on the game's
    /// own row instead (see `show_game_row`, mirroring the `[🔄 Updated]`
    /// decoration), so it stays visible on games where the row-level shield
    /// used to appear but does not need one to explain itself, and it must
    /// not appear on an unprotected game's row.
    #[test]
    fn game_row_shows_the_anti_cheat_badge_only_for_a_protected_game() {
        let mut test = UiTest::new(show);
        test.seed_many_findings(2, 1);
        {
            let app = test.app_mut();
            app.findings[0].row.anti_cheat_protected = true;
            app.rebuild_tree();
        }
        open_every_branch(&mut test);

        // Two games on screen, only one (game 0) anti-cheat protected: the
        // badge appearing exactly once - not zero, not on both - is what
        // proves it is scoped to the protected game's own row rather than
        // shown for every game or missing entirely.
        let badge = test.strings().badge_anticheat_shield;
        assert_eq!(
            test.count_labels_containing(badge),
            1,
            "the badge must be drawn on exactly one game row - the protected one"
        );
    }
}
