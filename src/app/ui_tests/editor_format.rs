//! Format in the Shell: the script indented by its brackets, on the offline demo.

use super::*;

#[test]
fn format_lays_out_the_shell_script() {
    let Some(mut ui) = demo_ui(|a| a.load_demo()) else { return };
    let t = ui.h.state_mut().tm().unwrap();
    t.view = View::Shell;
    t.shell.code = "async function run() {\nreturn await orders.find({ status: 'paid' }).toArray();\n}".into();
    ui.h.run_steps(2);
    ui.click("Format");
    ui.h.run_steps(1);
    assert_eq!(ui.app().t().unwrap().shell.code, "async function run() {\n  return await orders.find({ status: 'paid' }).toArray();\n}");
}
