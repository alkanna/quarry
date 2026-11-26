//! AI agents in the App: the settings, Data protection, Activity and approval windows. Agent calls are decided and
//! answered by Core (quarry-core/src/action/agents.rs); the windows send `AgentsA` actions.

use super::*;
use crate::agents::config::{Access, AgentsConfig};
use quarry_core::action::{Action, AgentsA};
use serde_json::json;
pub(crate) use quarry_core::state::agents::*;

mod activity;
mod approval;
mod conn_sheet;
mod demo;
mod rules;
mod settings;
pub(super) use rules::RulesWin;
#[cfg(test)]
pub(super) use rules::tester_lines;

impl App {
    pub(super) fn set_agents_enabled(&mut self, on: bool) {
        self.exec(Action::Agents(AgentsA::SetEnabled(on)));
    }

    /// Signs and saves the settings after a window changed them in place.
    pub(super) fn save_agents(&mut self) {
        self.exec(Action::Agents(AgentsA::Save));
    }

    /// "Changed by an agent" with Reload, over a tab whose collection or table an agent changed since it loaded.
    pub(super) fn agent_notice(&mut self, ui: &mut egui::Ui) {
        if !self.t().is_some_and(|t| t.agent_changed) {
            return;
        }
        ui.add_space(6.0);
        let mut reload = false;
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
            ui.painter().circle_filled(r.center(), STATUS_DOT_R, WARNING());
            ui.label(RichText::new("Changed by an agent since this loaded").size(TXT_SMALL).color(TEXT()));
            reload = ui.button(RichText::new("Reload").size(TXT_SMALL)).clicked();
        });
        if reload {
            self.reload();
        }
    }

    /// QUARRY_TEST_SETTINGS=agents: agents on with two shared demo connections and one client, never touching disk.
    pub(super) fn agents_demo(&mut self) {
        let core = self.core.setup();
        core.agents.cfg.enabled = true;
        for (i, access) in [Access::ReadWrite, Access::Read].into_iter().enumerate() {
            if let Some(c) = core.conns.get(i) {
                let p = crate::agents::config::ConnPolicy { access, ..core.agents.cfg.defaults.clone() };
                core.agents.cfg.conns.insert(c.meta.id, p);
            }
        }
        core.agents.link = LinkState::Listening;
        core.agents.sessions.push(AgentSession { id: 1, client: "claude-code".into(), calls: 14 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::config::{ConnPolicy, Mode};
    use crate::agents::link::{ClientInfo, ToolErr};
    use serde_json::Value;
    use tokio::sync::oneshot;

    /// Sends an open_shell call through the worker channel as the link does, and steps until it is answered.
    fn open_shell(h: &mut egui_kittest::Harness<'static, App>, args: Value) -> Result<Value, ToolErr> {
        let (reply, mut rx) = oneshot::channel();
        h.state().core.test_worker().send(Msg::Agent(db::AgentEvent::Call { session: 1, id: 1, tool: "open_shell".into(), args, client: ClientInfo { name: "claude-code".into(), version: "1".into() }, reply }));
        for _ in 0..20 {
            h.step();
            if let Ok(r) = rx.try_recv() {
                return r;
            }
        }
        panic!("open_shell was never answered");
    }

    /// The app tells Core what the window shows before each agent call: under an open dialog the tab opens behind it,
    /// and no edit lands under it.
    #[test]
    fn open_shell_sees_the_apps_open_dialog() {
        let mut h = egui_kittest::Harness::builder().with_accessibility_check(false).build_eframe(|cc| App::for_test(cc));
        {
            let app = h.state_mut();
            app.core.test_mut().conns.push(Conn::new(ConnMeta { id: 21, name: "prod".into(), ..Default::default() }, Status::Ready));
            app.core.test_mut().agents.cfg.enabled = true;
            app.core.test_mut().agents.cfg.conns.insert(21, ConnPolicy { access: Access::Read, scripts: Mode::Ask, ..Default::default() });
            app.modal = Some(Modal::Attach(Box::default()));
        }
        let script = "async function run() { return 1 }";
        let v = open_shell(&mut h, json!({ "connection": "prod", "database": "shop", "script": script })).unwrap();
        assert_eq!(v["shown"], false, "{v}");
        let e = open_shell(&mut h, json!({ "connection": "prod", "database": "shop", "script": script, "tab": v["tab"] })).unwrap_err();
        assert_eq!(e.code, "busy");
    }
}
