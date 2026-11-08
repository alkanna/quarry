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
            meta: ConnMeta { id: 1, name: "Local sample".into(), tag: "Local".into(), ..Default::default() },
            client: None,
            status: Status::Ready,
            writable: false,
            dbs,
            masked_uri: "mongodb://localhost:27017".into(),
            tunnel: None,
            server: ServerInfo { version: "7.0.12".into(), set_name: "rs0".into(), host: "localhost:27017".into() },
        });
        self.conns.push(Conn {
            meta: ConnMeta { id: 2, name: "Staging".into(), tag: "Staging".into(), ..Default::default() },
            client: None,
            status: Status::Idle,
            writable: false,
            dbs: Vec::new(),
            masked_uri: "mongodb+srv://app:****@staging.example.net".into(),
            tunnel: None,
            server: Default::default(),
        });
        self.active = Some(1);
        self.open_dbs.insert((1, "shop".into()));
        self.views.insert((1, "shop".into(), "paid_orders".into()));
        let sel = |db: &str, coll: &str| Sel { conn: 1, db: db.into(), coll: coll.into() };
        self.tabs.push(Tab::new(sel("shop", "customers")));
        let mut t = Tab::new(sel("shop", "orders"));
        t.fields = auto_fields(&docs);
        t.paths = doc_paths(&docs);
        t.field_info = field_info(&docs);
        t.docs = docs.iter().filter_map(|d| db::Row::from_doc(d).ok()).collect();
        t.count = 1284;
        t.ms = 14;
        t.stats = Some((1284, 412 * 1024, 3));
        t.selected = Some((false, 2));
        t.filter_txt = if std::env::var("QUARRY_TEST_AC").is_ok_and(|v| v == "date") { "{ createdAt: }".into() } else { "{ status: { $in: ['paid', 'shipped', 'pending'] } }".into() };
        t.sort_txt = "{ total: -1 }".into();
        t.query.sort = Some(doc! { "total": -1 });
        self.tabs.push(t);
        self.tabs.push(Tab::new(sel("analytics", "events")));
        self.cur = 1;
        if let Ok(mut log) = db::LOG.lock() {
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
        self.status = ("Offline demo: no database is connected.".into(), false);
        if std::env::var("QUARRY_TEST_CONNFORM").is_ok() {
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
        if std::env::var("QUARRY_TEST_INDEXES").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Indexes;
            t.selected = None;
            t.indexes = vec![
                doc! { "name": "_id_", "keys": { "_id": 1 } },
                doc! { "name": "number_1", "keys": { "number": 1 }, "unique": true },
                doc! { "name": "status_1_createdAt_-1", "keys": { "status": 1, "createdAt": -1 } },
                doc! { "name": "customer.email_1", "keys": { "customer.email": 1 }, "collation": { "locale": "en", "strength": 2 } },
                doc! { "name": "createdAt_1", "keys": { "createdAt": 1 }, "expireAfterSeconds": 7_776_000_i64, "hidden": true },
                doc! { "name": "notes_text", "keys": { "_fts": "text", "_ftsx": 1 }, "weights": { "notes": 1 } },
            ];
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
        if std::env::var("QUARRY_TEST_SETTINGS").is_ok() {
            self.modal = Some(Modal::Settings);
        }
        // Test switch: show the Shell view with a finished run in the demo picture.
        if std::env::var("QUARRY_TEST_SHELL").is_ok() {
            let t = &mut self.tabs[1];
            t.view = View::Shell;
            t.shell.code = "async function run() {\n  const n = await orders.countDocuments({ status: 'paid' });\n  console.log('paid orders:', n);\n  return await orders.find({ status: 'paid' }).project({ number: 1, total: 1 }).limit(3).toArray();\n}".into();
            t.shell.logs = vec![("log".into(), "paid orders: 321".into())];
            let docs: Vec<Bson> = t.docs.iter().take(3).map(db::Row::owned).map(|d| Bson::Document(doc! { "_id": d.get("_id").cloned().unwrap(), "number": d.get("number").cloned().unwrap(), "total": d.get("total").cloned().unwrap() })).collect();
            t.shell.set_value(Bson::Array(docs));
            t.shell.ms = Some(42);
            t.shell.help = std::env::var("QUARRY_TEST_HELP").is_ok();
        }
        // Test switch: show the "Group by and count" window.
        if std::env::var("QUARRY_TEST_GROUP").is_ok() {
            let rows = vec![doc! { "_id": "paid", "count": 321 }, doc! { "_id": "shipped", "count": 321 }, doc! { "_id": "pending", "count": 321 }, doc! { "_id": "refunded", "count": 321 }];
            let pipeline = db::group_count_pipeline("status", &Bson::String("paid".into()), Document::new()).unwrap_or_default();
            self.modal = Some(Modal::Grouped { sel: self.tabs[1].sel.clone(), path: "status".into(), pipeline, rows });
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
            self.drafts.insert(self.draft_key(&d), Draft { conn: 1, db: "shop".into(), coll: "orders".into(), original: d.clone(), current: changed.clone() });
            let items = changed.get("items").cloned().unwrap();
            self.json_edit = Some(JsonEdit { base: changed, path: vec!["items".into()], text: db::to_edit_value(&items), kind: "array", focus: false });
        }
    }
}
