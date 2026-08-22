//! Frame-cost budgets for the screens a user waits on.
//!
//! Separate from the per-panel test modules because a frame's cost is not any
//! one panel's: the settings dialog is drawn *over* the findings tree, which
//! keeps rebuilding its visible rows underneath it, so a measurement that
//! drove `ui::settings::show` alone would report a cost nobody pays. These
//! drive [`GameTrimmerApp::draw_frame`] - the same list `eframe::App::ui`
//! draws, in the same order.
//!
//! # Why a wall-clock budget, and why it is not tight
//!
//! Timing assertions are the classic flaky test, so these are shaped to fail
//! only on the thing that was actually reported: switching a settings section
//! taking *seconds*. The budget is roughly an order of magnitude above what a
//! healthy debug frame costs, measured on the minimum of several frames (see
//! [`UiTest::fastest_frame`]) - a busy machine cannot push the floor up, only
//! a slower frame can. `GT_FRAME_BUDGET_MS` overrides it for a machine slower
//! than the one this was calibrated on.
//!
//! The companion ratio assertion is the one that keeps working when the
//! absolute number does not: whatever a frame costs here, no single section
//! may cost several times what the others do.

use crate::app::GameTrimmerApp;
use crate::ui::harness::UiTest;
use crate::ui::settings::SettingsSection;

/// Findings behind the dialog. A real library scan produces thousands, and
/// the reported symptom only shows up with one loaded - the tree underneath
/// is redrawn on every repaint, whatever is on top of it.
const GAMES: usize = 200;
const FILES_PER_GAME: usize = 25;

/// Frames each measurement takes the floor of. Enough to skip the first one,
/// which pays for font atlas and layout caches that a running app filled in
/// long before the user opened this dialog.
const RUNS: usize = 5;

/// Per-frame ceiling, in milliseconds, for a debug build on the machine this
/// was calibrated on. Deliberately far above the measured cost: this exists to
/// catch "the dialog freezes for seconds", not to police small regressions.
const DEFAULT_BUDGET_MS: u128 = 400;

fn frame_budget_ms() -> u128 {
    std::env::var("GT_FRAME_BUDGET_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_BUDGET_MS)
}

/// The dialog open over a full findings tree, which is the state the slowdown
/// was reported in.
fn settings_over_a_full_tree() -> UiTest {
    let mut test = UiTest::new(GameTrimmerApp::draw_frame);
    test.seed_many_findings(GAMES, FILES_PER_GAME);
    test.app_mut().show_settings = true;
    test.run();
    test
}

/// The measured floor cost of one frame per section, in the order the nav
/// lists them.
fn frame_cost_per_section(test: &mut UiTest) -> Vec<(SettingsSection, u128)> {
    SettingsSection::ORDER
        .iter()
        .map(|&section| {
            test.app_mut().settings_section = section;
            test.run();
            (section, test.fastest_frame(RUNS).as_millis())
        })
        .collect()
}

/// The reported symptom, stated as a budget: no settings section may take
/// seconds to draw with a real scan behind it.
#[test]
fn every_settings_section_draws_a_frame_within_budget() {
    let mut test = settings_over_a_full_tree();
    let budget = frame_budget_ms();

    let costs = frame_cost_per_section(&mut test);

    let over: Vec<_> = costs.iter().filter(|(_, ms)| *ms > budget).collect();
    assert!(
        over.is_empty(),
        "with {} findings behind it, these settings sections cost more than the \
         {budget}ms budget for one frame: {over:?} (all sections: {costs:?})",
        GAMES * FILES_PER_GAME,
    );
}

/// The half that survives being run on a machine slower than the one the
/// budget above was written on: whatever a frame costs here, one section
/// costing several times what its neighbours do is a bug in that section, not
/// a slow computer.
#[test]
fn no_settings_section_costs_several_times_what_the_others_do() {
    /// Sections genuinely differ in content - "Scanning" is much the longest -
    /// so this allows a real spread and only catches an outlier.
    const MAX_RATIO: u128 = 6;
    /// Below this the measurement is mostly timer granularity, and a ratio
    /// between two such numbers means nothing.
    const NOISE_FLOOR_MS: u128 = 4;

    let mut test = settings_over_a_full_tree();

    let costs = frame_cost_per_section(&mut test);
    let cheapest = costs
        .iter()
        .map(|(_, ms)| *ms)
        .min()
        .expect("five sections");
    if cheapest < NOISE_FLOOR_MS {
        return;
    }

    for (section, ms) in &costs {
        assert!(
            *ms <= cheapest * MAX_RATIO,
            "{section:?} costs {ms}ms a frame, more than {MAX_RATIO}x the cheapest \
             section's {cheapest}ms - it is doing per-frame work the others are not \
             (all sections: {costs:?})",
        );
    }
}

/// The deterministic half of the budgets above, and the one that found the
/// slowdown they were written for: "Rules" read both pack files and rebuilt a
/// whole `RuleEngine` from one of them on *every* frame, so the section cost
/// as much as the slowest of four file reads - which on a cold disk or behind
/// an on-access virus scanner is where the reported seconds came from.
///
/// A count rather than a duration, so this says the same thing on any machine:
/// drawing many frames of one section may not multiply the reading it does.
#[test]
fn the_rules_section_does_not_re_read_its_packs_every_frame() {
    /// Two packs, so one pass over the section is two reads.
    const READS_PER_PASS: usize = 2;
    const FRAMES: usize = 20;

    let mut test = UiTest::new(GameTrimmerApp::draw_frame);
    test.app_mut().show_settings = true;
    test.app_mut().settings_section = SettingsSection::Rules;
    test.run();

    let started = std::time::Instant::now();
    for _ in 0..FRAMES {
        test.time_frame();
    }
    let elapsed = started.elapsed();

    // The cap is derived rather than constant, because the section is entitled
    // to consult the files again once `PACK_STATE_TTL` expires. A fixed `<= 2`
    // held only while the whole loop fitted inside one second, which it does
    // when the test runs alone and does not under `cargo test -j 4` - the
    // failure then reported a caching regression that had not happened. This
    // allows one extra pass per elapsed TTL and stays exact about the claim:
    // reads may not scale with frames.
    let passes_allowed = 1 + elapsed
        .as_nanos()
        .div_ceil(crate::ui::settings::rules::PACK_STATE_TTL.as_nanos())
        as usize;
    let allowed = READS_PER_PASS * passes_allowed;

    let reads = test.pack_disk_reads();
    assert!(
        reads <= allowed,
        "{FRAMES} frames of the Rules section went to disk {reads} times for its pack \
         readouts in {elapsed:?}, over the {allowed} its cache lifetime permits - it is \
         re-reading and re-parsing both packs per frame, which is what makes switching to \
         this tab take seconds on a slow disk",
    );
}

/// The other half of the same claim: the cache must not turn the readout into
/// a stale one. A restore rewrites the pack, so the line beside it has to
/// re-read rather than report what it saw before the click.
#[test]
fn restoring_a_pack_re_reads_its_readout_instead_of_reporting_a_cached_one() {
    let mut test = UiTest::new(GameTrimmerApp::draw_frame);
    test.app_mut().show_settings = true;
    test.app_mut().settings_section = SettingsSection::Rules;
    test.run();
    let before = test.pack_disk_reads();

    let s = test.strings();
    test.click(&format!(
        "{} \u{2014} {}",
        s.btn_restore_defaults,
        crate::worker::RULES_FILE_NAME,
    ));

    assert!(
        test.pack_disk_reads() > before,
        "the readout was still served from the cache after a restore rewrote the pack",
    );
}

/// The tree underneath is what makes a settings frame expensive at all, so
/// the dialog must not add a cost of its own on top of it that grows with the
/// findings. Compares the same frame with the dialog open and closed.
#[test]
fn opening_the_dialog_does_not_multiply_the_cost_of_a_frame() {
    /// The dialog is a modal over the same tree, so it may cost more - but a
    /// small multiple of it, not an order of magnitude.
    const MAX_RATIO: u128 = 4;
    const NOISE_FLOOR_MS: u128 = 4;

    let mut test = UiTest::new(GameTrimmerApp::draw_frame);
    test.seed_many_findings(GAMES, FILES_PER_GAME);
    test.run();
    let closed = test.fastest_frame(RUNS).as_millis();

    test.app_mut().show_settings = true;
    test.run();
    let open = test.fastest_frame(RUNS).as_millis();

    if closed < NOISE_FLOOR_MS {
        return;
    }
    assert!(
        open <= closed * MAX_RATIO,
        "a frame costs {closed}ms with the settings dialog closed and {open}ms with it \
         open - the dialog is adding more than {MAX_RATIO}x on top of the tree it covers",
    );
}
