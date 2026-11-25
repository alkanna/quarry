//! The Shell's Run button turning into Stop while a script runs, offline on the demo data.

use super::*;
use std::time::{Duration, Instant};

fn shell_demo() -> Option<UiApp> {
    demo_ui(|a| {
        a.load_demo();
        a.core.cur = 1;
        a.core.tabs[1].view = View::Shell;
    })
}

/// Marks the demo tab's script as running since `at`, with a cancel channel Stop can fire.
fn running_since(ui: &mut UiApp, at: Instant) {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    let sh = &mut ui.h.state_mut().core.tabs[1].shell;
    (sh.cancel, sh.started) = (Some(tx), Some(at));
    ui.h.run_steps(2);
}

#[test]
fn stop_takes_the_place_of_run_while_a_script_runs() {
    let Some(mut ui) = shell_demo() else { return };
    ui.show("Run");
    assert!(ui.h.query_all_by_label("Stop").next().is_none(), "Stop shows with nothing running");
    let run = ui.h.get_by_label("Run").rect();
    // A future start keeps the elapsed time at zero however slow the frames are.
    running_since(&mut ui, Instant::now() + Duration::from_secs(60));
    ui.show("Stop");
    assert!(ui.h.query_all_by_label("Run").next().is_none(), "Run stays beside Stop");
    assert_eq!(ui.h.get_by_label("Stop").rect(), run, "Stop does not sit exactly where Run was");
    ui.click("Stop");
    assert!(ui.app().core.tabs[1].shell.cancel.is_some(), "a click right after Run stopped the script");
    running_since(&mut ui, Instant::now() - Duration::from_secs(1));
    ui.click("Stop");
    assert!(ui.app().core.tabs[1].shell.cancel.is_none(), "Stop did not stop the script");
}
