//! QUARRY_TEST_AGENTS=<view>: the AI agents windows filled with made-up calls, rules and sessions, for screenshots and
//! the UI tests. Nothing here reads or writes the disk or the keychain.

use super::*;
use crate::agents::config::{Action, ConnPolicy, Matcher, Mode, PresetMode, RuleSpec, Scope, ScopeMode, Tri};
use crate::agents::policy::AskWhy;
use std::time::Instant;

fn rule(matcher: Matcher, scope: &[&str], action: Action) -> RuleSpec {
    RuleSpec { on: true, matcher, scope: scope.iter().map(|s| s.to_string()).collect(), action, mode: PresetMode::Hide }
}

fn act(id: u64, time: &str, tool: &str, conn: Option<u64>, ns: &str, outcome: Outcome, summary: &str) -> Activity {
    Activity { id, time: time.into(), session: 1, client: "Claude Code".into(), tool: tool.into(), conn, ns: ns.into(), outcome, summary: summary.into(), args: String::new(), answer: String::new(), undo: None }
}

impl App {
    /// The demo connections plus a MySQL and a PostgreSQL one, shared in different ways, with one agent connected.
    pub(in crate::app) fn agents_demo_view(&mut self, view: &str) {
        for (id, name, tag, engine) in [(3, "shop-prod", "Production", db::sql::Engine::MySql), (5, "analytics-prod", "Production", db::sql::Engine::Postgres)] {
            if self.conn(id).is_none() {
                let product = if engine == db::sql::Engine::MySql { "MySQL" } else { "PostgreSQL" };
                let mut c = Conn::new(ConnMeta { id, name: name.into(), tag: tag.into(), engine, ..Default::default() }, Status::Idle);
                c.server.product = product.into();
                self.conns.push(c);
            }
        }
        let cfg = &mut self.agents.cfg;
        cfg.enabled = true;
        let local = ConnPolicy {
            access: Access::ReadWrite,
            scripts: Mode::Never,
            scope: Scope { mode: ScopeMode::Except, globs: vec!["shop.audit_*".into(), "*.sessions".into(), "billing".into()] },
            presets: crate::agents::config::PresetOverride { secrets: Tri::Inherit, pii: Tri::Off },
            rules: vec![rule(Matcher::FieldPath("customers.**.loyalty_id".into()), &[], Action::Hash), rule(Matcher::ValueRegex(r"ORD-\d{8}".into()), &[], Action::Mask(4))],
            ..cfg.defaults.clone()
        };
        cfg.conns.insert(1, local);
        cfg.conns.insert(3, ConnPolicy { access: Access::Read, ..cfg.defaults.clone() });
        cfg.conns.insert(5, ConnPolicy { access: Access::Read, ..cfg.defaults.clone() });
        cfg.rules = vec![
            rule(Matcher::ValueRegex(r"\b\d{3}-\d{2}-\d{4}\b".into()), &[], Action::Redact),
            rule(Matcher::FieldPath("customers.**.ssn".into()), &["shop.*"], Action::Hash),
            RuleSpec { mode: PresetMode::MaskOutput, ..rule(Matcher::FieldName("^card_".into()), &["shop.*"], Action::Mask(4)) },
        ];
        cfg.presets.pii.off = vec!["pii.ip".into()];
        self.agents.link = LinkState::Listening;
        self.agents.sessions = vec![AgentSession { id: 1, client: "Claude Code".into(), calls: 14 }];
        let mut a = vec![
            act(1, "22:38:02", "list_connections", None, "", Outcome::Allowed, "3 shared"),
            act(2, "22:38:40", "describe", Some(1), "shop.orders", Outcome::Allowed, "24 fields from 1,000 documents"),
            act(3, "22:39:02", "sql_insert_rows", Some(3), "shop.customers", Outcome::Refused("Connection is read-only for agents".into()), ""),
            act(4, "22:39:30", "find", Some(2), "orders.items", Outcome::Refused("Not shared".into()), ""),
            act(5, "22:39:58", "drop_collection", Some(1), "shop.tmp_import", Outcome::Refused("Change structure is Never".into()), ""),
            act(6, "22:40:12", "select_rows", Some(5), "reporting.daily", Outcome::Allowed, "48 rows"),
            act(7, "22:40:51", "find", Some(1), "shop.orders", Outcome::Allowed, "200 of 3,120 rows"),
            act(8, "22:41:07", "update", Some(1), "shop.orders", Outcome::Approved, "412 changed"),
        ];
        a[7].args = "{\n  \"connection\": \"1\",\n  \"database\": \"shop\",\n  \"collection\": \"orders\",\n  \"filter\": { \"status\": \"pending\", \"createdAt\": { \"$lt\": \"2026-10-01\" } },\n  \"update\": { \"$set\": { \"status\": \"expired\" } },\n  \"many\": true\n}".into();
        a[7].answer = "{ \"matched\": 412, \"modified\": 412 }".into();
        a[7].undo = Some(8);
        self.agents.activity = a.into();
        let base = |id: u64, tool: &str, conn: u64, ns: &str, op: &str, code: &str| Ask {
            id,
            client: "Claude Code".into(),
            tool: tool.into(),
            conn,
            ns: ns.into(),
            op: op.into(),
            code: code.into(),
            facts: Vec::new(),
            reason: None,
            why: AskWhy { setting: true, always: false, unlock: false, connect: false, large: None, session_ok: true },
            no_undo: None,
            // Awake and part way through the countdown, as the designs show.
            at: Instant::now() - Duration::from_secs(8),
            shown: Some(Instant::now() - Duration::from_secs(5)),
            gone: false,
        };
        let update = || {
            let mut k = base(9, "update", 1, "shop.orders", "UPDATE MANY", "filter { status: \"pending\",\n         createdAt: { $lt: ISODate(\"2026-10-01\") } }\nupdate { $set: { status: \"expired\" } }");
            k.facts = vec!["Matches 412 documents, counted just now.".into()];
            k.why.large = Some(412);
            k.reason = Some("Expire orders that stayed pending for over a month, as you asked.".into());
            k
        };
        match view {
            "settings" => self.agents_demo_settings(),
            "conn" => {
                self.agents_demo_settings();
                self.agents_open_page(1);
            }
            "rules" => {
                self.agents_demo_settings();
                self.agents.rules = Some(RulesWin::default());
            }
            "activity" => {
                self.agents.activity_open = true;
                self.agents.picked = Some(8);
            }
            "approval" => self.agents.asks = vec![update()].into(),
            "approval-big" => {
                let mut k = base(10, "delete", 1, "shop.orders", "DELETE MANY", "filter { status: \"cancelled\" }");
                k.facts = vec!["Matches 4,812 documents, counted just now.".into()];
                k.no_undo = Some(String::new());
                k.why.session_ok = false;
                k.reason = Some("Clean up cancelled orders before the migration.".into());
                self.agents.asks = vec![k].into();
            }
            "approval-shell" => {
                let mut k = base(11, "run_shell", 1, "", "RUN SHELL", "const rows = await db.orders.find({ total: { $gt: 500 } })\n  .toArray();\nfor (const r of rows) print(r.customer, r.total);");
                k.why = AskWhy { setting: false, always: true, unlock: true, session_ok: false, ..k.why };
                k.reason = Some("List big orders to find the top customers.".into());
                self.agents.asks = vec![k].into();
            }
            "approval-locked" => {
                let mut k = base(12, "sql_update_rows", 3, "shop.customers", "UPDATE ROWS", "set   { tier: \"gold\" }\nwhere { id: [1042, 1043, 1077] }");
                k.facts = vec!["Matches 3 rows by primary key. Quarry keeps the old values to undo.".into()];
                k.why.unlock = true;
                k.reason = Some("Promote the three customers you named.".into());
                // Shown just now, so Allow is still asleep when the picture is taken.
                k.shown = Some(Instant::now() + Duration::from_secs(60));
                self.agents.asks = vec![k].into();
            }
            "approval-gone" => {
                let mut k = update();
                k.gone = true;
                self.agents.asks = vec![k].into();
            }
            "queue" => {
                let mut second = base(13, "insert", 1, "shop.orders", "INSERT", "[{ number: 10501, status: \"paid\" }]");
                second.facts = vec!["1 document.".into()];
                let third = base(14, "create_index", 1, "shop.orders", "CREATE INDEX", "{ createdAt: -1 }");
                self.agents.asks = vec![update(), second, third].into();
            }
            // "pill" and anything else: the status bar pill over the documents.
            _ => {}
        }
    }

    fn agents_demo_settings(&mut self) {
        self.modal = Some(Modal::Settings);
        self.settings_cat = super::super::settings::SettingsCat::Agents;
    }
}
