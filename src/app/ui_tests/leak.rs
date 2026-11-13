// Scratch measurement, not for commit: memory after repeated open and close cycles.
use super::*;
use mongodb::bson::doc;

#[cfg(windows)]
fn mem() -> (u64, u64) {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        let mut m: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        GetProcessMemoryInfo(GetCurrentProcess(), &mut m as *mut _ as *mut _, m.cb);
        (m.WorkingSetSize as u64 >> 20, m.PrivateUsage as u64 >> 20)
    }
}
// Linux: resident set, and bytes glibc malloc has handed out and not had back.
#[cfg(target_os = "linux")]
fn mem() -> (u64, u64) {
    let rss = std::fs::read_to_string("/proc/self/statm").ok().and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok()).unwrap_or(0) * 4096;
    let live = unsafe { libc::mallinfo2() }.uordblks as u64;
    (rss >> 20, live >> 20)
}
#[cfg(not(any(windows, target_os = "linux")))]
fn mem() -> (u64, u64) {
    (0, 0)
}

fn report(ui: &UiApp, what: &str) {
    let ctx = &ui.h.ctx;
    let atlas = ctx.fonts(|f| f.font_image_size());
    let data = ctx.memory(|m| m.data.len());
    let tex = ctx.tex_manager().read().num_allocated();
    let a = ui.app();
    let closed_rows: usize = a.closed.iter().map(|t| t.docs.len()).sum();
    let (ws, private) = mem();
    eprintln!("{what:<22} rss {ws:>4} MB heap {private:>4} MB atlas {atlas:?} idmap {data} tabs {} closed {} ({closed_rows} rows)", a.tabs.len(), a.closed.len());
}

fn big_tab(sel: Sel) -> Tab {
    let mut t = Tab::new(sel);
    let pad = "x".repeat(900);
    t.docs = (0..2000).filter_map(|i| db::Row::from_doc(&doc! { "_id": i, "name": format!("row {i}"), "pad": &pad }).ok()).collect();
    t
}

#[test]
fn memory_after_open_close_cycles() {
    let Some(mut ui) = demo_ui(App::load_demo) else { return };
    ui.h.run_steps(10);
    report(&ui, "start");
    let sel = ui.app().tabs[1].sel.clone();
    for cycle in 0..8 {
        {
            let a = ui.h.state_mut();
            for _ in 0..4 {
                a.tabs.push(big_tab(sel.clone()));
            }
            a.set_cur(1);
        }
        ui.h.run_steps(3);
        for cat in ["appearance", "reading", "documents", "editing", "tools", "status", "updates"] {
            let a = ui.h.state_mut();
            a.modal = Some(Modal::Settings);
            a.settings_cat = settings::SettingsCat::named(cat);
            ui.h.run_steps(3);
        }
        ui.h.state_mut().modal = Some(Modal::Palette { query: String::new(), sel: 0, cmds: true, new_tab: false });
        ui.h.run_steps(3);
        ui.h.state_mut().modal = Some(Modal::Relations { conn: 1, db: "shop".into() });
        ui.h.run_steps(3);
        ui.h.state_mut().server = Some(ServerWin::demo(1, cycle % 2 == 0));
        ui.h.run_steps(3);
        ui.h.state_mut().server = None;
        ui.h.state_mut().modal = None;
        for v in [View::Indexes, View::Schema, View::Shell, View::Aggregate, View::Compare, View::Import, View::Watch, View::Documents] {
            let a = ui.h.state_mut();
            a.tabs[1].view = v;
            a.tabs[1].selected = None;
            ui.h.run_steps(3);
        }
        report(&ui, &format!("cycle {cycle} all open"));
        while !ui.app().tabs.is_empty() {
            ui.h.state_mut().close_tab(0);
        }
        ui.h.run_steps(5);
        report(&ui, &format!("cycle {cycle} all closed"));
        // Back to a demo-like state for the next cycle.
        let a = ui.h.state_mut();
        for _ in 0..2 {
            a.tabs.push(Tab::new(sel.clone()));
        }
        a.set_cur(1);
    }
    ui.h.state_mut().closed.clear();
    ui.h.run_steps(5);
    report(&ui, "closed list cleared");
    #[cfg(target_os = "linux")]
    unsafe { libc::malloc_trim(0) };
    report(&ui, "after malloc_trim");
}
