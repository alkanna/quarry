//! Format in the Query view on PostgreSQL: a function with a dollar quoted body is laid out, the body stays byte for byte, and Cmd+Z
//! brings the old text back.

use super::sql_edit::every_engine;
use super::*;

const SRC: &str = "create or replace function f(a int) returns int language plpgsql as $fn$ begin  return a::int+1 ;  end $fn$; select a,b from t where c=$1 and d->>'k'='x'";
const BODY: &str = "$fn$ begin  return a::int+1 ;  end $fn$";

fn query(a: &App) -> String {
    a.t().and_then(|t| t.sql.as_ref()).map(|s| s.query.clone()).unwrap_or_default()
}

#[test]
fn format_keeps_a_dollar_quoted_body_and_undo_restores_the_text() {
    every_engine("format keeps a dollar quoted body", |fx, ui| {
        if !fx.is_pg() {
            return;
        }
        let (db, _) = fx.place();
        ui.h.state_mut().side_act(CONN, SideAct::NewQuery(db.to_string()));
        ui.wait("the Query tab", |a| a.t().is_some_and(|t| t.sql.is_some()));
        ui.h.state_mut().tabs.iter_mut().filter_map(|t| t.sql.as_mut()).for_each(|s| s.query = SRC.into());
        ui.settle();
        ui.click("Format");
        let new = query(ui.app());
        assert_ne!(new, SRC, "the statement was laid out");
        assert!(new.contains('\n') && new.contains(BODY), "{new}");
        assert!(new.contains("$1") && new.contains("->>") && !new.contains(": :"), "{new}");
        // The button took the focus off the editor, so it is given back before the key.
        let id = App::sql_editor_id(ui.app().cur);
        ui.h.ctx.memory_mut(|m| m.request_focus(id));
        ui.settle();
        ui.h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
        ui.settle();
        assert_eq!(query(ui.app()), SRC, "Cmd+Z restores the text before Format");
    });
}
