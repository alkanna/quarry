//! The demo connection with sample data, used for screenshots and first runs.

use super::*;

impl App {
    pub(super) fn load_demo(&mut self) {
        let names = ["Ada Byron", "Grace Hopper", "Linus Ito", "Mira Okafor", "Sam Patel", "Noor Haddad", "Jun Park", "Eva Lind"];
        let status = ["paid", "shipped", "pending", "refunded"];
        let docs: Vec<Document> = (0..50)
            .map(|i: i32| {
                doc! {
                    "_id": ObjectId::parse_str(format!("6650f1c2a1b2c3d4e5f6{:04x}", 0x0700 + i)).unwrap(),
                    "number": 10_400 + i,
                    "customer": { "name": names[i as usize % names.len()], "tier": if i % 3 == 0 { "gold" } else { "standard" } },
                    "status": status[i as usize % status.len()],
                    "total": 19.5 + (i as f64 * 7.25) % 230.0,
                    "items": (0..(1 + i % 4)).map(|k| Bson::Document(doc! { "sku": format!("SKU-{:03}", 100 + k * 7 + i), "qty": 1 + k })).collect::<Vec<_>>(),
                    "gift": i % 5 == 0,
                    "note": if i % 4 == 1 { Bson::Null } else { Bson::String("leave at the door".into()) },
                    "createdAt": mongodb::bson::DateTime::from_millis(1_730_000_000_000 + i as i64 * 3_600_000),
                    "updatedAt": 1_730_000_000i64 + i as i64 * 3600,
                }
            })
            .collect();
        let colls = |v: &[&str]| Some(v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let dbs = vec![
            ("admin".to_string(), colls(&["system.users", "system.version"])),
            ("analytics".to_string(), colls(&["events", "funnels", "sessions"])),
            ("config".to_string(), colls(&["system.sessions"])),
            ("local".to_string(), colls(&["startup_log"])),
            ("shop".to_string(), colls(&["carts", "customers", "orders", "paid_orders", "products", "reviews", "shipments"])),
            ("users".to_string(), colls(&["accounts", "profiles"])),
        ];
        self.conns.push(Conn {
            dbs,
            masked_uri: "mongodb://localhost:27017".into(),
            server: ServerInfo { version: "7.0.12".into(), set_name: "rs0".into(), host: "localhost:27017".into(), ..Default::default() },
            ..Conn::new(ConnMeta { id: 1, name: "Local sample".into(), tag: "Local".into(), ..Default::default() }, Status::Ready)
        });
        self.conns.push(Conn { masked_uri: "mongodb+srv://app:****@staging.example.net".into(), ..Conn::new(ConnMeta { id: 2, name: "Staging".into(), tag: "Staging".into(), ..Default::default() }, Status::Idle) });
        self.active = Some(1);
        self.open_dbs.insert((1, "shop".into()));
        self.views.add(1, "shop", "paid_orders");
        let sel = |db: &str, coll: &str| Sel { conn: 1, db: db.into(), coll: coll.into(), ..Default::default() };
        self.tabs.push(Tab::new(sel("shop", "customers")));
        let mut t = Tab::new(sel("shop", "orders"));
        t.fields = auto_fields(&docs);
        t.paths = doc_paths(&docs);
        t.field_info = field_info(&docs).into();
        t.docs = docs.iter().filter_map(|d| db::Row::from_doc(d).ok()).collect();
        t.count = 1284;
        t.ms = 14;
        t.stats = Some((1284, 412 * 1024, 6));
        t.selected = Some((false, 2));
        t.filter_txt = if std::env::var("QUARRY_TEST_AC").is_ok_and(|v| v == "date") { "{ createdAt: }".into() } else { "{ status: { $in: ['paid', 'shipped', 'pending'] } }".into() };
        t.sort_txt = "{ total: -1 }".into();
        t.query.sort = Some(doc! { "total": -1 });
        t.limit_txt = "50".into();
        self.tabs.push(t);
        self.tabs.push(Tab::new(sel("analytics", "events")));
        self.cur = 1;
        // QUARRY_TEST_CROWD=1 adds long collection names and thirty connections, for the one-line rows and the scrolling connections menu.
        if std::env::var("QUARRY_TEST_CROWD").is_ok() {
            if let Some((_, Some(c))) = self.conns[0].dbs.iter_mut().find(|d| d.0 == "shop") {
                c.extend(["hydra_oauth2_obfuscated_authentication_session", "hydra_oauth2_trusted_jwt_bearer_issuer"].map(String::from));
                c.sort();
            }
            for i in 0..30u64 {
                self.conns.push(Conn::new(ConnMeta { id: 100 + i, name: format!("cluster-{i:02}"), ..Default::default() }, Status::Idle));
            }
        }
        // QUARRY_TEST_RAIL=1 adds the MySQL demo connection and a connecting one beside the Mongo demo, for the rail shortcuts.
        if std::env::var("QUARRY_TEST_RAIL").is_ok() && std::env::var("QUARRY_TEST_SQL").is_err() {
            // The SQL demo replaces the tabs, which the Mongo demo keeps.
            let tabs = std::mem::take(&mut self.tabs);
            self.load_sql_demo();
            self.tabs = tabs;
            self.cur = 1;
            self.active = Some(1);
            self.conns.push(Conn { masked_uri: "mysql://app:****@reports.internal:3306".into(), ..Conn::new(ConnMeta { id: 4, name: "reports".into(), tag: "Staging".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Connecting) });
        }
        // QUARRY_TEST_PG=<view> does the same with an offline PostgreSQL connection, see demo_pg.rs.
        if std::env::var("QUARRY_TEST_PG").is_ok() {
            self.load_pg_demo();
            return;
        }
        // QUARRY_TEST_SQL=content swaps the demo for a MySQL connection with its orders table open.
        if std::env::var("QUARRY_TEST_SQL").is_ok() {
            self.load_sql_demo();
            return;
        }
        {
            let mut log = db::log();
            let q = r#"{ "status": { "$in": ["paid", "shipped", "pending"] } }"#;
            log.extend([
                db::LogEntry::finished(1, 340, "admin", "listDatabases", r#"{ "listDatabases": 1, "nameOnly": true }"#, 4, None),
                db::LogEntry::finished(1, 338, "shop", "listCollections", r#"{ "listCollections": 1, "nameOnly": true }"#, 3, None),
                db::LogEntry::finished(1, 212, "shop", "aggregate", r#"{ "aggregate": "orders", "pipeline": [{ "$match": { "status": { "$in": ["paid", "shipped"] } } }, { "$group": { "_id": "$customer.tier", "orders": { "$sum": 1 }, "revenue": { "$sum": "$total" } } }] }"#, 31, None),
                db::LogEntry::finished(1, 95, "shop", "insert", r#"{ "insert": "orders", "documents": [{ "_id": { "$oid": "6650f1c2a1b2c3d4e5f60702" }, "number": 10402 }] }"#, 6, Some("E11000 duplicate key error collection: shop.orders index: _id_ dup key")),
                db::LogEntry::finished(1, 41, "shop", "count", &format!(r#"{{ "count": "orders", "query": {q} }}"#), 9, None),
                db::LogEntry::finished(1, 40, "shop", "find", &format!(r#"{{ "find": "orders", "filter": {q}, "sort": {{ "total": -1 }}, "limit": 50 }}"#), 14, None),
            ]);
        }
        // QUARRY_TEST_LOG docks the query log under the documents, as in the README's main picture.
        self.log.open = std::env::var("QUARRY_TEST_LOG").is_ok();
        let key = self.col_key(false);
        self.prefs.cols.insert(key, ColPrefs { pinned: vec!["status".into()], highlight: vec!["customer.name".into()], ..Default::default() });
        self.status = "Offline demo: no database is connected.".into();
        if std::env::var("QUARRY_TEST_CONNFORM").as_deref() == Ok("mysql") {
            // QUARRY_TEST_CONNFORM_TAB=advanced opens the Advanced tab with the design's example values.
            let advanced = std::env::var("QUARRY_TEST_CONNFORM_TAB").as_deref() == Ok("advanced");
            let mut form = ConnForm {
                id: Some(2),
                name: "shop-prod".into(),
                tag: "Production".into(),
                engine: db::sql::Engine::MySql,
                addr: db::sql::Addr { scheme: "mysql".into(), host: "shop-prod.internal".into(), port: 3306, user: "app".into(), password: "hunter2hunter".into(), database: "shop".into(), ..Default::default() },
                ssh_on: true,
                ssh: crate::ssh::SshConf { host: "bastion-eu.example.com".into(), port: 22, user: "deploy".into(), auth: crate::ssh::SshAuth::Agent, ..Default::default() },
                ..Default::default()
            };
            if advanced {
                use db::sql::advanced::MODE_FLAGS;
                let mode = ["STRICT_TRANS_TABLES", "NO_ZERO_DATE", "NO_ZERO_IN_DATE", "ERROR_FOR_DIVISION_BY_ZERO", "NO_ENGINE_SUBSTITUTION"];
                debug_assert!(mode.iter().all(|m| MODE_FLAGS.contains(m)));
                form.set_advanced(&db::sql::Advanced {
                    zone: db::sql::Zone::Custom("+02:00".into()),
                    sql_mode: Some(mode.map(String::from).to_vec()),
                    init: "SET SESSION group_concat_max_len = 1048576;\nSET SESSION innodb_lock_wait_timeout = 10;\nSET SESSION sql_safe_updates = 1;".into(),
                    connect_secs: Some(15),
                    query_secs: Some(5),
                    ..Default::default()
                });
                form.tab = ConnTab::Advanced;
            }
            self.modal = Some(Modal::Conn(Box::new(form)));
        } else if std::env::var("QUARRY_TEST_CONNFORM").is_ok() {
            self.modal = Some(Modal::Conn(Box::new(ConnForm {
                id: Some(2),
                name: "Staging".into(),
                tag: "Staging".into(),
                uri: "mongodb://app:secret@10.0.3.12:27017/?replicaSet=rs0".into(),
                ssh_on: true,
                ssh: crate::ssh::SshConf { host: "bastion.example.com".into(), port: 22, user: "deploy".into(), auth: crate::ssh::SshAuth::KeyFile, key_path: "~/.ssh/id_ed25519".into(), legacy_rsa: false, known_host: Some("SHA256:3n9Yk0mF1q2hV7Qp6zXcR4tL8wE5sA0dJ2uB1nM7kPo".into()) },
                ..Default::default()
            })));
        }
        if std::env::var("QUARRY_TEST_USERS").is_ok() {
            self.users = Some(UsersWin {
                conn: 1,
                users: Some(Ok(vec![
                    doc! { "user": "admin", "db": "admin", "roles": [ { "role": "root", "db": "admin" } ] },
                    doc! { "user": "shop_app", "db": "shop", "roles": [ { "role": "readWrite", "db": "shop" } ] },
                    doc! { "user": "analyst", "db": "admin", "roles": [ { "role": "read", "db": "shop" }, { "role": "read", "db": "analytics" } ] },
                    // Many roles, to check the list wraps instead of widening the window.
                    doc! { "user": "reporting", "db": "admin", "roles": (0..24).map(|i| Bson::Document(doc! { "role": "read", "db": format!("tenant-api-customer-{i:02}") })).collect::<Vec<_>>() },
                ])),
                roles: Some(Ok(vec![doc! { "role": "reportsReader", "db": "admin", "roles": [ { "role": "read", "db": "analytics" } ] }])),
                new_db: "admin".into(),
                ..Default::default()
            });
        }
        if std::env::var("QUARRY_TEST_WATCH").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Watch;
            t.selected = None;
            // A live-looking stream: the receiver is dropped, so nothing ever arrives.
            t.watch.cancel = Some(tokio::sync::oneshot::channel().0);
            let at = |s: i64| mongodb::bson::DateTime::from_millis(1_730_000_000_000 + s * 1000);
            let id = |n: u8| ObjectId::from_bytes([0x66, 0x50, 0xf1, 0xc2, 0xa1, 0xb2, 0xc3, 0xd4, 0xe5, 0xf6, 0x08, n]);
            let events = [
                doc! { "operationType": "insert", "wallTime": at(0), "documentKey": { "_id": id(1) }, "fullDocument": { "_id": id(1), "number": 10501, "status": "pending", "total": 42.5 } },
                doc! { "operationType": "update", "wallTime": at(4), "documentKey": { "_id": id(1) }, "updateDescription": { "updatedFields": { "status": "paid", "paidAt": at(4) }, "removedFields": [] }, "fullDocument": { "_id": id(1), "number": 10501, "status": "paid", "total": 42.5, "paidAt": at(4) } },
                doc! { "operationType": "insert", "wallTime": at(9), "documentKey": { "_id": id(2) }, "fullDocument": { "_id": id(2), "number": 10502, "status": "pending", "total": 18.0 } },
                doc! { "operationType": "update", "wallTime": at(15), "documentKey": { "_id": id(2) }, "updateDescription": { "updatedFields": { "total": 21.0 }, "removedFields": ["coupon"] }, "fullDocument": { "_id": id(2), "number": 10502, "status": "pending", "total": 21.0 } },
                doc! { "operationType": "delete", "wallTime": at(22), "documentKey": { "_id": id(3) } },
            ];
            t.watch.rows = events.into_iter().map(watch_row).filter_map(|d| db::Row::from_doc(&d).ok()).collect();
        }
        // QUARRY_TEST_SERVER=1 opens the Server window, =ops on its Operations section.
        if let Ok(v) = std::env::var("QUARRY_TEST_SERVER") {
            self.server = Some(ServerWin::demo(1, v == "ops"));
        }
        // The orders tab's indexes, for the Indexes view and the indexed-field marks.
        self.tabs[1].set_indexes(vec![
            doc! { "name": "_id_", "keys": { "_id": 1 } },
            doc! { "name": "number_1", "keys": { "number": 1 }, "unique": true },
            doc! { "name": "status_1_createdAt_-1", "keys": { "status": 1, "createdAt": -1 } },
            doc! { "name": "customer.email_1", "keys": { "customer.email": 1 }, "collation": { "locale": "en", "strength": 2 } },
            doc! { "name": "createdAt_1", "keys": { "createdAt": 1 }, "expireAfterSeconds": 7_776_000_i64, "hidden": true },
            doc! { "name": "notes_text", "keys": { "_fts": "text", "_ftsx": 1 }, "weights": { "notes": 1 } },
        ]);
        if std::env::var("QUARRY_TEST_INDEXES").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Indexes;
            t.selected = None;
            let now = mongodb::bson::DateTime::now().timestamp_millis();
            let day = 24 * 3600 * 1000;
            let stat = |name: &str, ops: i64, since: i64, size: usize| db::IndexStat { name: name.into(), ops, since: Some(since), hidden: name == "createdAt_1", size: Some(size) };
            let shape = db::Shape { filter: doc! { "total": { "$gt": 100 } }, sort: doc! { "createdAt": -1 }, count: 214, avg_ms: 380.0, suggestion: doc! { "createdAt": -1, "total": 1 } };
            self.idx_info = Some((
                "shop.orders".into(),
                db::IndexInfo {
                    stats_err: None,
                    stats: vec![stat("_id_", 120_431, now - 40 * day, 2_310_144), stat("number_1", 8_204, now - 40 * day, 1_048_576), stat("status_1_createdAt_-1", 96_310, now - 40 * day, 3_407_872), stat("customer.email_1", 0, now - 40 * day, 1_572_864), stat("createdAt_1", 0, now - 2 * day, 917_504), stat("notes_text", 12, now - 40 * day, 5_242_880)],
                    level: 1,
                    slowms: 100,
                    shapes: vec![shape],
                },
            ));
            self.refresh_idx_rows("shop.orders");
            // QUARRY_TEST_INDEXES=new also opens the New index dialog.
            if std::env::var("QUARRY_TEST_INDEXES").is_ok_and(|v| v == "new") {
                self.open_create_index(Some(doc! { "createdAt": -1, "total": 1 }));
            }
        }
        if std::env::var("QUARRY_TEST_SCHEMA").is_ok() {
            self.tabs[1].view = View::Schema;
            self.tabs[1].selected = None;
        }
        // Test switch: a long generated validator, to check its box scrolls inside the window.
        if std::env::var("QUARRY_TEST_VALIDATOR").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Schema;
            t.selected = None;
            let props: Document = (0..60).map(|i| (format!("field{i}"), Bson::Document(doc! { "bsonType": "string", "description": "generated" }))).collect();
            t.schema.vtext = db::to_edit_json(&doc! { "$jsonSchema": { "bsonType": "object", "properties": props } });
        }
        if std::env::var("QUARRY_TEST_IMPORT").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Import;
            t.selected = None;
            t.import.kind = db::ImportKind::Csv;
            t.import.file = Some("new-orders.csv".into());
            t.import.text = "number,customer,status,total,createdAt\n10501,Ada Byron,paid,42.5,2024-11-02T10:00:00Z\n10502,Grace Hopper,pending,18,2024-11-02T11:30:00Z\n10503,Linus Ito,shipped,99.9,2024-11-03T08:15:00Z\n".into();
        }
        // QUARRY_TEST_SETTINGS=1 opens Settings; a category's first word (reading, tools...) opens that one.
        if let Ok(cat) = std::env::var("QUARRY_TEST_SETTINGS") {
            self.modal = Some(Modal::Settings);
            self.settings_cat = settings::SettingsCat::named(&cat);
            if self.settings_cat == settings::SettingsCat::Agents {
                self.agents_demo();
            }
        }
        // QUARRY_TEST_AGENTS=<view> fills the AI agents windows with made-up calls, see agents/demo.rs.
        if let Ok(v) = std::env::var("QUARRY_TEST_AGENTS") {
            self.agents_demo_view(&v);
        }
        // Test switch: show the Shell view with a finished run in the demo picture. QUARRY_TEST_VIM shows it too, with
        // Vim on and the editor in that mode (see vim::demo).
        let vim = std::env::var("QUARRY_TEST_VIM").is_ok();
        self.prefs.vim |= vim;
        if std::env::var("QUARRY_TEST_SHELL").is_ok() || vim {
            let t = &mut self.tabs[1];
            t.view = View::Shell;
            t.shell.code = "async function run() {\n  const n = await orders.countDocuments({ status: 'paid' });\n  console.log('paid orders:', n);\n  return await orders.find({ status: 'paid' }).project({ number: 1, total: 1 }).limit(3).toArray();\n}".into();
            t.shell.logs = vec![("log".into(), "paid orders: 321".into())];
            let docs: Vec<Bson> = t.docs.iter().take(3).map(db::Row::owned).map(|d| Bson::Document(doc! { "_id": d.get("_id").cloned().unwrap(), "number": d.get("number").cloned().unwrap(), "total": d.get("total").cloned().unwrap() })).collect();
            t.shell.set_value(Bson::Array(docs));
            t.shell.ms = Some(42);
            t.shell.help = std::env::var("QUARRY_TEST_HELP").is_ok();
        }
        // Test switch: the shell with parameter hints and a suggestion list, see shell_sig_demo.
        if let Some(code) = shell_ls::shell_sig_code().filter(|_| std::env::var("QUARRY_TEST_SQL").is_err()) {
            let t = &mut self.tabs[1];
            t.view = View::Shell;
            t.shell.code = code;
        }
        // Test switch: show the "Group by and count" window.
        if std::env::var("QUARRY_TEST_GROUP").is_ok() {
            let rows = vec![doc! { "_id": "paid", "count": 321 }, doc! { "_id": "shipped", "count": 321 }, doc! { "_id": "pending", "count": 321 }, doc! { "_id": "refunded", "count": 321 }];
            let pipeline = db::group_count_pipeline("status", &Bson::String("paid".into()), Document::new()).unwrap_or_default();
            self.modal = Some(Modal::Grouped { sel: self.tabs[1].sel.clone(), path: "status".into(), pipeline, rows });
        }
        // Test switch: the Cmd+K palette with this query; a leading ">" opens command mode.
        if let Ok(q) = std::env::var("QUARRY_TEST_PALETTE") {
            self.modal = Some(Modal::Palette { query: q.trim_start_matches('>').into(), sel: 0, cmds: q.starts_with('>'), new_tab: false });
        }
        // Test switch: show the pipeline builder with stage previews.
        if std::env::var("QUARRY_TEST_PIPELINE").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Aggregate;
            t.selected = None;
            let docs = t.decoded_docs();
            let done = |mut s: Stage, n: u64, sample: Vec<Document>, ms: u128| {
                (s.count, s.sample, s.ms, s.due) = (Some(n), sample, Some(ms), None);
                s
            };
            t.agg_input = done(Stage::new("", "", true), 1284, docs.iter().take(10).cloned().collect(), 3);
            let paid: Vec<Document> = docs.iter().filter(|d| matches!(d.get_str("status"), Ok("paid" | "shipped"))).take(10).cloned().collect();
            let groups = vec![doc! { "_id": "standard", "orders": 428, "revenue": 51210.75 }, doc! { "_id": "gold", "orders": 214, "revenue": 25431.5 }];
            t.stages = vec![
                done(Stage::new("$match", "{ status: { $in: ['paid', 'shipped'] } }", true), 642, paid, 12),
                done(Stage::new("$group", "{\n  _id: '$customer.tier',\n  orders: { $sum: 1 },\n  revenue: { $sum: '$total' }\n}", true), 2, groups.clone(), 31),
                done(Stage::new("$sort", "{ revenue: -1 }", true), 2, groups, 33),
            ];
            t.stages[1].open.insert(0);
            t.agg_txt = text_from_stages(&t.stages);
            t.stages_src = t.agg_txt.clone();
        }
        // Test switch: show the Compare view with a finished comparison.
        if std::env::var("QUARRY_TEST_COMPARE").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Compare;
            t.selected = None;
            (t.compare.conn, t.compare.db, t.compare.coll) = (1, "shop".into(), "paid_orders".into());
            t.compare.ran = (1, "shop".into(), "paid_orders".into());
            let src = t.decoded_docs();
            let mut dst: Vec<Document> = src.iter().skip(3).cloned().collect();
            for d in dst.iter_mut().take(4) {
                d.insert("status", "refunded");
                d.remove("note");
            }
            dst.push(doc! { "_id": ObjectId::parse_str("6650f1c2a1b2c3d4e5f6ffff").unwrap(), "number": 9999, "status": "test" });
            t.compare.set_result(Some(db::compare_docs(src, dst)));
        }
        // Test switch: show the Data relations window with a few relations.
        if std::env::var("QUARRY_TEST_RELATIONS").is_ok() {
            let r = |coll: &str, path: &str, target: &str, preview: &str| Relation { coll: coll.into(), path: path.into(), target: target.into(), preview: preview.into() };
            self.prefs.relations.insert("1/shop".into(), vec![r("orders", "customerId", "customers", "name"), r("orders", "items.productId", "products", "title"), r("reviews", "authorAccountId", "customers", "email")]);
            self.modal = Some(Modal::Relations { conn: 1, db: "shop".into() });
        }
        // Test switch: show the Dump dialog.
        if std::env::var("QUARRY_TEST_DUMP").is_ok() {
            self.modal = Some(Modal::Tool(Box::new(ToolForm { conn: 1, db: "shop".into(), from_db: "shop".into(), restore: true, path: "/Users/me/Downloads/shop.archive.gz".into(), gzip: true, ..Default::default() })));
        }
        // Test switch: show the JSON editor and an unsaved edit in the demo picture.
        if std::env::var("QUARRY_TEST_EDIT").is_ok() {
            let d = self.tabs[1].docs[2].owned();
            let mut changed = d.clone();
            changed.insert("status", "shipped");
            self.drafts.insert(self.draft_key(&d), Draft { conn: 1, db: "shop".into(), schema: String::new(), coll: "orders".into(), original: d.clone(), current: changed.clone() });
            let items = changed.get("items").cloned().unwrap();
            self.json_edit = Some(JsonEdit { focus: false, ..JsonEdit::new(changed, vec!["items".into()], db::to_edit_value(&items), "array") });
        }
        // QUARRY_TEST_ERR shows an error state: view, timeout, bar, toast, log, details or conn.
        if let Ok(v) = std::env::var("QUARRY_TEST_ERR") {
            self.demo_error(&v);
        }
        // QUARRY_TEST_DIALOG=<name> opens one of the smaller dialogs, to check its look.
        if let Ok(v) = std::env::var("QUARRY_TEST_DIALOG") {
            self.demo_dialog(&v);
        }
    }

    pub(super) fn demo_dialog(&mut self, which: &str) {
        let sel = self.tabs[1].sel.clone();
        let docs: Vec<Document> = self.tabs[1].decoded_docs().into_iter().take(3).collect();
        self.modal = Some(match which {
            "unlock" => Modal::Unlock(1, None),
            "delete" => Modal::Delete(sel, docs),
            "typed" => Modal::Typed { title: "Drop the collection orders in shop? Its 1,284 documents and 4 indexes are deleted for good.".into(), expect: "orders".into(), typed: "ord".into(), action: Danger::DropCollection },
            "newcoll" => Modal::NewCollection { conn: 1, db: "shop".into(), name: "invoice_lines".into(), view: false, source: String::new(), pipeline: String::new() },
            "newview" => Modal::NewCollection { conn: 1, db: "shop".into(), name: "orders_paid".into(), view: true, source: "orders".into(), pipeline: "[\n  { $match: { status: 'paid' } },\n  { $project: { total: 1, paidAt: 1 } }\n]".into() },
            "rename" => Modal::RenameColl { conn: 1, db: "shop".into(), coll: "orders".into(), name: "orders_2024".into() },
            "duplicate" => Modal::DuplicateColl { conn: 1, db: "shop".into(), coll: "orders".into(), name: "customers".into() },
            "save" => {
                let d = docs[0].clone();
                let mut c = d.clone();
                c.insert("status", "shipped");
                c.insert("gift", true);
                Modal::Save { tag: String::new(), changes: db::diff(&d, &c) }
            }
            "shell" => Modal::ShellWrite(vec!["deleteMany".into(), "drop".into()]),
            "agg" => Modal::AggWrite(vec![]),
            "column" => Modal::ColumnCustomize { key: String::new(), agg: false, col: "customer.name".into(), label: "Customer".into(), path: "customer.name".into() },
            "text" => Modal::Text { title: "Query help".into(), body: "{ status: 'paid' }            exact value\n{ total: { $gt: 100 } }       comparison\n{ tags: { $in: ['a', 'b'] } } any of".into(), code: true },
            "update" => Modal::UpdateMany { set_txt: "{ status: 'archived' }".into(), unset_txt: "note".into(), preview: None },
            "undo" => Modal::Undo,
            _ => return,
        });
    }

    fn demo_error(&mut self, which: &str) {
        const REFUSED: &str = r#"Command failed: Error code 13 (Unauthorized): not authorized on shop to execute command { find: "orders", filter: { status: "paid" }, limit: 50, lsid: { id: UUID("3f1c9e2a-41c8-4bb4-9b1e-6f3c2d1a0b7e") }, $db: "shop" }"#;
        const DUP: &str = r#"An error occurred when trying to execute a write operation: WriteError(WriteError { code: 11000, code_name: None, message: "E11000 duplicate key error collection: shop.orders index: number_1 dup key: { number: 10402 }", details: None })"#;
        let i = self.cur;
        let sel = self.tabs.get(i).map(|t| t.sel.clone());
        // The demo connection is local and anonymous; a user shows what the panels say about privileges.
        self.conns[0].masked_uri = "mongodb+srv://report_ro:****@cluster0.mongodb.net/?authSource=admin".into();
        self.conns[0].server.host = "cluster0.mongodb.net".into();
        match which {
            "view" | "timeout" => {
                let raw = if which == "view" { REFUSED } else { "Command failed: Error code 50 (MaxTimeMSExpired): operation exceeded time limit" };
                self.tabs[i].docs.clear();
                self.tabs[i].count = 0;
                self.fail_view(i, View::Documents, raw.into(), "load the documents");
            }
            "bar" => {
                self.tabs[i].filter_txt = "{ status: 'paid', total: { $gt: 50 } }}".into();
                self.run_query();
            }
            "toast" => {
                self.err(DUP);
                self.warn_ro();
                self.toast_ok("Saved 1 document".into(), None);
            }
            "log" | "details" => {
                self.errors_open = which == "log";
                self.err("Could not write orders.csv: No space left on device (os error 28)");
                self.err(DUP);
                let e = self.log_err(REFUSED, Some(1), sel, "Documents · orders".into(), "load the documents");
                self.toasts.clear();
                if which == "details" {
                    self.modal = Some(Modal::ErrorDetails(Box::new(e)));
                }
            }
            "conn" => {
                self.conns[0].status = Status::Failed("Command failed: Error code 18 (AuthenticationFailed): Authentication failed.".into());
                self.tabs.clear();
                self.cur = 0;
            }
            _ => {}
        }
    }

    /// The MySQL demo: a production connection and an open orders table. Nothing here reads the Keychain or the network.
    pub(super) fn load_sql_demo(&mut self) {
        use db::sql::{Col, value::Kind};
        let which = std::env::var("QUARRY_TEST_SQL").unwrap_or_default();
        let which = which.as_str();
        let mut shop: Vec<String> = ["customers", "invoices", "order_items", "orders", "payments", "products", "shipments", "order_totals", "top_customers"].map(String::from).to_vec();
        shop.sort();
        self.conns.push(Conn {
            dbs: vec![("information_schema".into(), None), ("mysql".into(), None), ("shop".into(), Some(shop)), ("analytics".into(), None), ("sys".into(), None)],
            masked_uri: "mysql://app:****@shop-prod.internal:3306/shop".into(),
            server: ServerInfo { version: "8.4.3".into(), product: "MySQL".into(), host: "shop-prod.internal:3306".into(), tls: true, ..Default::default() },
            ..Conn::new(ConnMeta { id: 3, name: "shop-prod".into(), tag: "Production".into(), engine: db::sql::Engine::MySql, ..Default::default() }, Status::Ready)
        });
        self.active = Some(3);
        self.open_dbs.insert((3, "shop".into()));
        // The demo never lists tables, so every database counts as asked.
        for d in ["information_schema", "mysql", "analytics", "sys"] {
            self.sql_asked.insert((3, d.into()));
        }
        for v in ["order_totals", "top_customers"] {
            self.views.add(3, "shop", v);
        }
        let sel = |coll: &str| Sel { conn: 3, db: "shop".into(), coll: coll.into(), ..Default::default() };
        let engine = db::sql::Engine::MySql;
        let col = |name: &str, ty: &str, nullable: bool| Col::plain(name, ty, Kind::from_type_name(ty.split('(').next().unwrap_or(ty)), nullable);
        let cols = vec![col("id", "INT UNSIGNED", false), col("customer_id", "INT", false), col("status", "ENUM('pending','paid','shipped','refunded','cancelled')", false), col("total", "DECIMAL(10,2)", false), col("currency", "CHAR(3)", false), col("note", "TEXT", true), col("created_at", "DATETIME", false)];
        // id, customer_id, status, total, currency, note, created_at
        let data: [(i64, i64, &str, &str, &str, Option<&str>, &str); 13] = [
            (4410, 1284, "paid", "112.50", "EUR", None, "2026-11-09 12:41:07"),
            (4409, 877, "shipped", "26.75", "EUR", Some("Leave at the door"), "2026-11-09 12:38:52"),
            (4408, 1102, "shipped", "34.00", "USD", Some("Ring twice"), "2026-11-09 12:30:19"),
            (4407, 61, "paid", "1204.10", "EUR", Some("Gift wrap, two boxes"), "2026-11-09 11:58:44"),
            (4406, 1284, "refunded", "8.90", "GBP", None, "2026-11-09 11:51:02"),
            (4405, 430, "paid", "54.20", "EUR", Some("Call before delivery"), "2026-11-09 11:47:30"),
            (4404, 919, "shipped", "71.00", "USD", None, "2026-11-09 11:22:15"),
            (4403, 12, "paid", "15.25", "EUR", None, "2026-11-09 11:09:58"),
            (4402, 1190, "pending", "220.00", "EUR", Some("B2B, invoice monthly"), "2026-11-09 10:57:41"),
            (4401, 877, "paid", "9.99", "USD", None, "2026-11-09 10:44:03"),
            (4400, 305, "shipped", "63.40", "EUR", None, "2026-11-09 10:31:27"),
            (4399, 2201, "paid", "37.80", "USD", None, "2026-11-09 10:12:50"),
            (4398, 742, "cancelled", "45.00", "EUR", None, "2026-11-09 09:58:36"),
        ];
        let mut t = Tab::new(sel("orders")).for_engine(engine);
        t.docs = data
            .iter()
            .filter_map(|&(id, cust, status, total, cur, note, at)| db::Row::from_doc(&doc! { "_id": { "id": id }, "id": id, "customer_id": cust, "status": status, "total": total, "currency": cur, "note": note.map_or(Bson::Null, Bson::from), "created_at": at }).ok())
            .collect();
        t.fields = cols.iter().map(|c| c.name.clone()).collect();
        if let Some(s) = t.sql.as_mut() {
            (s.cols, s.pk) = (Arc::new(cols), vec!["id".into()]);
        }
        (t.count, t.ms, t.size) = (4410, 18, self.page_size());
        t.sort_txt = "created_at DESC".into();
        t.filter_txt = "status = 'paid' AND total > 50".into();
        // QUARRY_TEST_SQL=rules, sqlmode or grouped pick the query bar's state; anything else is the plain Content view.
        match which {
            "rules" => {
                t.filter_txt = "`status` IN ('paid', 'shipped') AND `total` > 50".into();
                t.sort_txt = "`created_at` DESC, `total` ASC".into();
                if let Some(s) = t.sql.as_mut() {
                    s.bar.c.limit = "100".into();
                }
            }
            "sqlmode" => {
                t.filter_txt = "status IN ('paid','shipped') AND total > 50".into();
                t.sort_txt = "created_at DESC, total ASC".into();
                if let Some(s) = t.sql.as_mut() {
                    s.bar.c.sql = true;
                    s.bar.c.group = "status, cu".into();
                    s.bar.c.limit = "100".into();
                }
            }
            "colpick" => {
                if let Some(s) = t.sql.as_mut() {
                    s.bar.preset = super::sql::bar::Preset { order: true, pick: Some("stat".into()), ..Default::default() };
                }
            }
            "ac" => {
                t.filter_txt = "status = 'paid' AND total > 50 AND cur".into();
                if let Some(s) = t.sql.as_mut() {
                    s.bar.preset.ac = true;
                    s.bar.c.sql = true;
                    s.bar.c.limit = "100".into();
                }
            }
            "grouped" => {
                t.filter_txt = "`status` IN ('paid', 'shipped') AND `total` > 50".into();
                t.sort_txt = "`revenue` DESC".into();
                let col = |name: &str, ty: &str| Col::plain(name, ty, Kind::from_type_name(ty), false);
                let rows: [(&str, &str, i64, &str, &str); 5] = [("paid", "EUR", 61, "6912.40", "113.32"), ("shipped", "EUR", 24, "2301.75", "95.91"), ("paid", "USD", 22, "1980.00", "90.00"), ("shipped", "USD", 14, "1204.10", "86.01"), ("paid", "GBP", 12, "840.25", "70.02")];
                t.docs = rows.iter().filter_map(|&(st, cur, n, rev, avg)| db::Row::from_doc(&doc! { "status": st, "currency": cur, "orders": n, "revenue": rev, "avg_total": avg }).ok()).collect();
                t.fields = ["status", "currency", "orders", "revenue", "avg_total"].map(String::from).to_vec();
                t.count = 9;
                t.selected = Some((false, 1));
                if let Some(s) = t.sql.as_mut() {
                    s.bar.table_cols = std::mem::take(&mut s.cols);
                    s.cols = Arc::new(vec![col("status", "ENUM"), col("currency", "CHAR"), col("orders", "BIGINT"), col("revenue", "DECIMAL"), col("avg_total", "DECIMAL")]);
                    s.pk.clear();
                    s.matched = Some(148);
                    s.bar.c.group = "`status`, `currency`".into();
                    s.bar.c.select = "`status`, `currency`, COUNT(*) AS `orders`, SUM(`total`) AS `revenue`, AVG(`total`) AS `avg_total`".into();
                    s.bar.c.having = "`orders` > 10".into();
                }
            }
            _ => {}
        }
        self.tabs = vec![t, Tab::new(sel("customers")).for_engine(engine)];
        self.cur = 0;
        self.status = "Offline demo: no database is connected.".into();
        self.sql_structure_demo(which);
        self.sql_alter_demo(which);
        self.sql_rel_demo(which);
        self.sql_trig_demo(which);
        self.sql_users_demo(which);
        // QUARRY_TEST_SQL=processes, variables, overview or status opens the SQL Server window on that tab.
        if let v @ ("processes" | "variables" | "overview" | "status") = which {
            self.sql_server = Some(SqlServerWin::demo(3, v));
        }
        if matches!(which, "query" | "log" | "format" | "library") {
            self.sql_query_demo();
        }
        if which == "log" {
            self.sql_log_demo();
        }
        if matches!(which, "shell" | "shellac" | "shellattach" | "attach") {
            self.sql_shell_demo();
        }
        if matches!(which, "ac" | "acquery" | "shellac" | "relations") {
            self.sql_ac_demo();
        }
        // QUARRY_TEST_SQL=edit stages two cells of row 3; =select checks four rows.
        match which {
            "edit" => self.sql_demo_stage(),
            "select" => self.tabs[0].checked = [1, 2, 4, 5].into(),
            // QUARRY_TEST_SQL=record shows row 2 as the record Tab opens.
            "record" => (self.tabs[0].selected, self.tabs[0].record) = (Some((false, 1)), true),
            _ => {}
        }
        self.sql_json_demo();
        // QUARRY_TEST_SQL=import or dump open their dialogs over the orders table.
        match which {
            "import" => self.demo_sql_import(),
            "dump" => self.demo_sql_dump(),
            _ => {}
        }
        self.sql_create_demo(which);
    }

    /// QUARRY_TEST_SQL_JSON=<file> swaps the demo rows of the orders grid (content, rules, select, edit) or of
    /// the first Query result set for `{"cols":[{"name","ty"}],"rows":[[text or null]]}`. Rows are converted
    /// like the worker's, so memory matches real use. Offline only: nothing here reads the keychain.
    fn sql_json_demo(&mut self) {
        let Some(v) = std::env::var("QUARRY_TEST_SQL_JSON").ok().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()) else { return };
        use db::sql::{value::Kind, Col};
        let cols: Vec<Col> = v["cols"].as_array().into_iter().flatten().map(|c| { let ty = c["ty"].as_str().unwrap_or("TEXT"); Col::plain(c["name"].as_str().unwrap_or(""), ty, Kind::from_column_type(ty), true) }).collect();
        let query = std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("query");
        let rows: Vec<db::Row> = v["rows"].as_array().into_iter().flatten().filter_map(|r| {
            let vals: Vec<Bson> = cols.iter().zip(r.as_array()?).map(|(c, x)| db::sql::value::to_bson(db::sql::Dialect::MySql, c.kind, x.as_str().map(str::as_bytes))).collect();
            let mut d = Document::new();
            if !query { d.insert("_id", doc! { cols[0].name.clone(): vals[0].clone() }); }
            for (c, x) in cols.iter().zip(vals) { d.insert(c.name.clone(), x); }
            db::Row::from_doc(&d).ok()
        }).collect();
        if query {
            if let Some(set) = self.tabs.last_mut().and_then(|t| t.sql.as_mut()).and_then(|s| s.sets.first_mut()) { (set.cols, set.rows) = (cols, rows); }
        } else if let Some(t) = self.tabs.first_mut() {
            t.fields = cols.iter().map(|c| c.name.clone()).collect();
            t.count = rows.len() as u64;
            t.docs = rows;
            if let Some(s) = t.sql.as_mut() { (s.cols, s.pk) = (Arc::new(cols.clone()), vec![cols[0].name.clone()]); }
        }
    }

    /// QUARRY_TEST_SQL=ac or acquery: the schema and column values the suggestions come from, and for `acquery`
    /// a Query tab with the caret after `c.`. The list is opened by the bar preset (see `open_list`).
    pub(super) fn sql_ac_demo(&mut self) {
        use db::sql::schema::{Schema, SchemaCol, SchemaTable};
        use db::sql::value::Kind;
        let col = |name: &str, ty: &str, key: &str, fk: bool| SchemaCol { name: name.into(), ty: ty.into(), kind: Kind::from_type_name(ty.split('(').next().unwrap_or(ty).trim_end_matches(" unsigned")), key: key.into(), nullable: false, fk };
        let table = |name: &str, rows: Option<u64>, cols: Vec<SchemaCol>| SchemaTable { name: name.into(), view: rows.is_none(), rows, cols };
        let orders = table("orders", Some(4410), vec![col("id", "int unsigned", "PRI", false), col("customer_id", "int", "MUL", true), col("status", "enum('pending','paid','shipped','refunded','cancelled')", "", false), col("total", "decimal(10,2)", "", false), col("currency", "char(3)", "", false), col("note", "text", "", false), col("created_at", "datetime", "", false)]);
        let customers = table("customers", Some(1284), vec![col("id", "int unsigned", "PRI", false), col("email", "varchar(255)", "UNI", false), col("name", "varchar(120)", "", false), col("country", "char(2)", "", false), col("created_at", "datetime", "", false)]);
        let order_items = table("order_items", Some(9120), vec![col("order_id", "int unsigned", "PRI", true), col("sku", "varchar(40)", "PRI", false)]);
        // A view has no row count, which is how the completion list tells it from a table.
        let order_totals = table("order_totals", None, vec![col("order_id", "int unsigned", "", false), col("total", "decimal(10,2)", "", false)]);
        let payments = table("payments", Some(4377), vec![col("id", "int unsigned", "PRI", false), col("order_id", "int unsigned", "MUL", true)]);
        let shipments = table("shipments", Some(3120), vec![col("id", "int unsigned", "PRI", false), col("order_id", "int unsigned", "MUL", true)]);
        self.put_sql_schema(3, "shop".into(), Arc::new(Schema { tables: vec![customers, order_items, orders, payments, shipments, order_totals] }));
        let mut vals = Fields::new();
        vals.insert(crate::ui::sql_complete::value_key("orders", "currency"), FieldInfo { kind: "string", values: ["EUR", "USD", "GBP"].map(Bson::from).to_vec(), freq: vec![61, 27, 12] });
        vals.insert(crate::ui::sql_complete::value_key("orders", "status"), FieldInfo { kind: "string", values: ["paid", "shipped", "pending", "refunded", "cancelled"].map(Bson::from).to_vec(), freq: vec![61, 22, 9, 5, 3] });
        self.sql_values.insert((3, "shop".into()), vals);
        if std::env::var("QUARRY_TEST_SQL").as_deref() == Ok("acquery") {
            let mut t = Tab::new(Sel { conn: 3, db: "shop".into(), coll: String::new(), ..Default::default() }).for_engine(db::sql::Engine::MySql);
            t.view = View::Sql;
            if let Some(s) = t.sql.as_mut() {
                s.query = "SELECT o.id, c.name FROM orders o\nJOIN customers c ON c.".into();
                s.caret = s.query.chars().count();
                s.bar.preset.ac = true;
            }
            self.tabs.push(t);
            self.cur = self.tabs.len() - 1;
        }
    }
}
