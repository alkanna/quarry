//! The PostgreSQL demo for QUARRY_TEST_PG=<view>: an offline "analytics-prod" connection with its orders table open, the way
//! QUARRY_TEST_SQL builds the MySQL one. Nothing here reads the keychain or the network. The views have the names of the MySQL
//! demo where the feature exists on PostgreSQL, plus `schemas` (the switcher open), `connform`, `paste` and `tls`.

use super::*;
use db::sql::schema::{Schema, SchemaCol, SchemaTable};
use db::sql::structure::{Column, Constraint, ForeignKey, Index, IndexCol, Structure, Trigger};
use db::sql::value::Kind;
use db::sql::{Col, PgSchema};

const CONN: u64 = 3;
const DB: &str = "analytics";
const SCHEMA: &str = "public";
const TABLES: [&str; 5] = ["customers", "events", "order_items", "orders", "products"];
const VIEWS: [&str; 2] = ["daily_revenue", "mv_cohorts"];
const ORDERS_ROWS: u64 = 1_204_331;

/// The orders columns: name, the type as `format_type` writes it, the sqlx name the grid shows, the pg_type category and typtype
/// the kind comes from, and whether NULL is allowed.
const ORDERS: [(&str, &str, &str, &str, &str, bool); 11] = [
    ("id", "bigint", "INT8", "N", "b", false),
    ("customer_id", "bigint", "INT8", "N", "b", false),
    ("public_id", "uuid", "UUID", "U", "b", false),
    ("status", "order_status", "order_status", "E", "e", false),
    ("total", "numeric(10,2)", "NUMERIC", "N", "b", false),
    ("total_cents", "bigint", "INT8", "N", "b", false),
    ("gift", "boolean", "BOOL", "B", "b", false),
    ("tags", "text[]", "TEXT[]", "A", "b", false),
    ("meta", "jsonb", "JSONB", "U", "b", false),
    ("note", "text", "TEXT", "S", "b", true),
    ("created_at", "timestamp with time zone", "TIMESTAMPTZ", "D", "b", false),
];

/// The kinds a real server reports for the demo columns, so the grid aligns and colors them as it does there.
fn orders_cols() -> Vec<Col> {
    ORDERS
        .iter()
        .map(|&(name, _, ty, cat, typtype, nullable)| {
            let mut c = Col::plain(name, ty, Kind::from_pg(ty, cat, typtype), nullable);
            (c.auto_increment, c.generated) = (name == "id", name == "total_cents");
            c
        })
        .collect()
}

/// A row of the orders grid as the worker converts it: numbers as ints, everything else as the server's own text.
struct Order {
    id: i64,
    customer: i64,
    status: &'static str,
    total: &'static str,
    gift: bool,
    tags: &'static str,
    meta: &'static str,
    note: Option<&'static str>,
    at: &'static str,
}

const fn order(id: i64, customer: i64, status: &'static str, total: &'static str, gift: bool, tags: &'static str, meta: &'static str, note: Option<&'static str>, at: &'static str) -> Order {
    Order { id, customer, status, total, gift, tags, meta, note, at }
}

const ROWS: [Order; 13] = [
    order(1204331, 48120, "paid", "112.50", false, "{}", r#"{"channel": "web"}"#, None, "2026-11-09 12:41:07+00"),
    order(1204330, 9033, "shipped", "26.75", false, "{prepaid}", r#"{"channel": "app", "coupon": "AUTUMN10"}"#, Some("Leave at the door"), "2026-11-09 12:38:52+00"),
    order(1204329, 120554, "shipped", "34.00", false, "{}", r#"{"channel": "web"}"#, Some("Ring twice"), "2026-11-09 12:30:19+00"),
    order(1204328, 61, "paid", "1204.10", true, "{gift,priority}", r#"{"channel": "sales", "boxes": 2}"#, Some("Gift wrap, two boxes"), "2026-11-09 11:58:44+00"),
    order(1204327, 48120, "refunded", "8.90", false, "{}", r#"{"channel": "web", "refund": "damaged"}"#, None, "2026-11-09 11:51:02+00"),
    order(1204326, 30417, "paid", "54.20", false, "{prepaid}", r#"{"channel": "app"}"#, Some("Call before delivery"), "2026-11-09 11:47:30+00"),
    order(1204325, 9033, "shipped", "71.00", false, "{}", r#"{"channel": "web"}"#, None, "2026-11-09 11:22:15+00"),
    order(1204324, 77902, "paid", "15.25", false, "{}", r#"{"channel": "web"}"#, None, "2026-11-09 11:09:58+00"),
    order(1204323, 120554, "pending", "220.00", false, "{b2b,invoice}", r#"{"channel": "sales", "terms": "net30"}"#, Some("B2B, invoice monthly"), "2026-11-09 10:57:41+00"),
    order(1204322, 9033, "paid", "9.99", false, "{}", r#"{"channel": "app"}"#, None, "2026-11-09 10:44:03+00"),
    order(1204321, 5512, "shipped", "63.40", false, "{prepaid}", r#"{"channel": "web"}"#, None, "2026-11-09 10:31:27+00"),
    order(1204320, 22010, "paid", "37.80", true, "{gift}", r#"{"channel": "web"}"#, None, "2026-11-09 10:12:50+00"),
    order(1204319, 7420, "cancelled", "45.00", false, "{}", r#"{"channel": "app"}"#, None, "2026-11-09 09:58:36+00"),
];

/// The uuid of an order. Version 7 in the first group of the third block, as the column default would generate.
fn public_id(id: i64) -> String {
    format!("{:08x}-7c1e-7b2a-9d3f-{:012x}", 0x0192_f3a0 + (id % 4096), id * 7919)
}

/// The stored total_cents of a total such as "112.50", which the generated column computes as total * 100.
fn cents(total: &str) -> i64 {
    let (whole, frac) = total.split_once('.').unwrap_or((total, "0"));
    whole.parse::<i64>().unwrap_or(0) * 100 + format!("{frac:0<2}")[..2].parse::<i64>().unwrap_or(0)
}

impl Order {
    fn doc(&self) -> Document {
        doc! { "_id": { "id": self.id }, "id": self.id, "customer_id": self.customer, "public_id": public_id(self.id), "status": self.status, "total": self.total, "total_cents": cents(self.total), "gift": self.gift, "tags": self.tags, "meta": self.meta, "note": self.note.map_or(Bson::Null, Bson::from), "created_at": self.at }
    }
}

/// The names a schema lists in the sidebar, tables and views together and sorted as the server's list is.
fn names(tables: &[&str], views: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = tables.iter().chain(views).map(|s| s.to_string()).collect();
    v.sort();
    v
}

/// The schemas of the database as the switcher lists them, system ones included for its toggle.
fn schemas() -> Vec<PgSchema> {
    let s = |name: &str, tables: u64, system: bool| PgSchema { name: name.into(), tables, system };
    vec![s("information_schema", 69, true), s("pg_catalog", 64, true), s(SCHEMA, (TABLES.len() + VIEWS.len()) as u64, false), s("reporting", 4, false), s("staging", 3, false)]
}

impl App {
    /// QUARRY_TEST_PG=schemas has no pointer to click with, so the demo opens the schema menu itself once the first frames settled.
    pub(super) fn demo_opens_schema_menu(&self, ctx: &egui::Context) -> bool {
        self.demo && (60..63).contains(&ctx.cumulative_frame_nr()) && std::env::var("QUARRY_TEST_PG").as_deref() == Ok("schemas")
    }

    /// The PostgreSQL demo: a production connection with three databases, three schemas in `analytics` and an open orders table.
    pub(super) fn load_pg_demo(&mut self) {
        let which = std::env::var("QUARRY_TEST_PG").unwrap_or_default();
        let which = which.as_str();
        let by_schema = HashMap::from([
            (SCHEMA.to_string(), names(&TABLES, &VIEWS)),
            ("reporting".to_string(), names(&["cohort_retention", "revenue_by_region", "weekly_kpis"], &["v_active_accounts"])),
            ("staging".to_string(), names(&["raw_events", "raw_orders", "raw_refunds"], &[])),
        ]);
        let tables = HashMap::from([(DB.to_string(), by_schema)]);
        let bare = |d: &str| (d.to_string(), None);
        self.conns.push(Conn {
            tables,
            dbs: vec![(DB.into(), Some(vec![SCHEMA.into(), "reporting".into(), "staging".into()])), bare("postgres"), bare("billing")],
            masked_uri: "postgresql://analyst:****@analytics-prod.internal:5432/analytics".into(),
            server: ServerInfo { version: "18.1".into(), product: "PostgreSQL".into(), host: "analytics-prod.internal:5432".into(), tls: true, ..Default::default() },
            ..Conn::new(ConnMeta { id: CONN, name: "analytics-prod".into(), tag: "Production".into(), engine: db::sql::Engine::Postgres, ..Default::default() }, Status::Ready)
        });
        self.active = Some(CONN);
        self.open_dbs.insert((CONN, DB.into()));
        // The demo never lists anything, so every database and schema counts as asked.
        for d in [DB, "postgres", "billing"] {
            self.sql_asked.insert((CONN, d.into()));
        }
        for s in [SCHEMA, "reporting", "staging"] {
            self.pg_tables_asked.insert(CONN, DB, s);
        }
        if let Some(c) = self.conns.last_mut() {
            c.pg.insert(DB.into(), PgNs { schemas: schemas(), picked: SCHEMA.into() });
        }
        for v in VIEWS {
            self.views.add(CONN, &ns_key(DB, SCHEMA), v);
        }
        self.views.add(CONN, &ns_key(DB, "reporting"), "v_active_accounts");
        self.pg_schema_demo();

        let sel = |coll: &str| Sel { conn: CONN, db: DB.into(), schema: SCHEMA.into(), coll: coll.into() };
        let engine = db::sql::Engine::Postgres;
        let cols = orders_cols();
        let mut t = Tab::new(sel("orders")).for_engine(engine);
        t.docs = ROWS.iter().filter_map(|o| db::Row::from_doc(&o.doc()).ok()).collect();
        t.fields = cols.iter().map(|c| c.name.clone()).collect();
        if let Some(s) = t.sql.as_mut() {
            (s.cols, s.pk, s.estimated) = (Arc::new(cols), vec!["id".into()], true);
        }
        (t.count, t.ms, t.size) = (ORDERS_ROWS, 18, self.page_size());
        t.sort_txt = "created_at DESC".into();
        t.filter_txt = "status = 'paid' AND total > 50".into();
        pg_bar_demo(which, &mut t);
        self.tabs = vec![t, Tab::new(sel("customers")).for_engine(engine)];
        self.cur = 0;
        self.status = "Offline demo: no database is connected.".into();
        self.pg_structure_demo(which);
        match which {
            "query" => self.pg_query_demo(),
            "shell" => self.pg_shell_demo(),
            "acquery" => self.pg_acquery_demo(),
            "edit" => self.sql_demo_stage(),
            "select" => self.tabs[0].checked = [1, 2, 4, 5].into(),
            "connform" | "paste" | "tls" => self.pg_form_demo(which),
            _ => {}
        }
    }

    /// The autocomplete schema of the public schema, which also gives the sidebar its row counts, and the values the
    /// suggestions offer for the status column.
    fn pg_schema_demo(&mut self) {
        let col = |name: &str, ty: &str, kind: Kind, key: &str, fk: bool| SchemaCol { name: name.into(), ty: ty.into(), kind, key: key.into(), nullable: false, fk };
        let int = |n: &str, key: &str, fk: bool| col(n, "bigint", Kind::Int, key, fk);
        let text = |n: &str| col(n, "text", Kind::Text, "", false);
        let ts = |n: &str| col(n, "timestamp with time zone", Kind::DateTime, "", false);
        let table = |name: &str, rows: Option<u64>, cols: Vec<SchemaCol>| SchemaTable { name: name.into(), view: rows.is_none(), rows, cols };
        let orders: Vec<SchemaCol> = ORDERS.iter().map(|&(n, ty, _, cat, typtype, _)| col(n, ty, Kind::from_pg(ty.split('(').next().unwrap_or(ty), cat, typtype), match n { "id" => "PRI", "public_id" => "UNI", "customer_id" => "MUL", _ => "" }, n == "customer_id")).collect();
        let schema = Schema {
            tables: vec![
                table("customers", Some(120_554), vec![int("id", "PRI", false), col("email", "text", Kind::Text, "UNI", false), text("name"), col("country", "character(2)", Kind::Text, "", false), ts("created_at")]),
                table("daily_revenue", None, vec![col("day", "date", Kind::Date, "", false), col("revenue", "numeric", Kind::Decimal, "", false)]),
                table("events", Some(48_227_640), vec![int("id", "PRI", false), int("order_id", "MUL", true), text("kind"), col("payload", "jsonb", Kind::Json, "", false), ts("at")]),
                table("mv_cohorts", None, vec![col("cohort", "date", Kind::Date, "", false), col("customers", "bigint", Kind::Int, "", false)]),
                table("order_items", Some(3_418_902), vec![int("order_id", "PRI", true), text("sku"), col("qty", "integer", Kind::Int, "", false)]),
                table("orders", Some(ORDERS_ROWS), orders),
                table("products", Some(2_318), vec![text("sku"), text("title"), col("price", "numeric(10,2)", Kind::Decimal, "", false)]),
            ],
        };
        self.put_sql_schema(CONN, ns_key(DB, SCHEMA), Arc::new(schema));
        let mut vals = Fields::new();
        vals.insert(crate::ui::sql_complete::value_key("orders", "status"), FieldInfo { kind: "string", values: ["paid", "shipped", "pending", "refunded", "cancelled"].map(Bson::from).to_vec(), freq: vec![61, 22, 9, 5, 3] });
        self.sql_values.insert((CONN, ns_key(DB, SCHEMA)), vals);
    }

    /// The orders table of the design as the Structure, Info, Relations and Triggers views show it: a bigint identity
    /// key, a unique uuid, a CHECK, a foreign key to customers, a generated column and a trigger, as step 4 reads them.
    fn pg_structure_demo(&mut self, which: &str) {
        let view = match which {
            "structure" => View::Structure,
            "info" => View::Info,
            "relations" => View::Relations,
            "triggers" => View::Triggers,
            _ => return,
        };
        let Some(t) = self.tabs.first_mut() else { return };
        t.view = view;
        t.selected = None;
        if let Some(s) = t.sql.as_mut() {
            s.st.data = Some(orders_structure());
            s.st.sel = [Some(3), None, Some(0), None, Some(0)];
        }
    }

    /// The Query tab of the design, with a script of two reads and a refused write.
    fn pg_query_demo(&mut self) {
        let mut t = Tab::new(Sel { conn: CONN, db: DB.into(), schema: SCHEMA.into(), coll: String::new() }).for_engine(db::sql::Engine::Postgres);
        t.view = View::Sql;
        let col = |name: &str, ty: &str, kind: Kind| Col::plain(name, ty, kind, false);
        let people: [(&str, &str, i64, &str); 6] = [("Ada Byron", "ada@example.com", 6, "1412.40"), ("Grace Hopper", "grace@example.com", 5, "988.15"), ("Ken Thompson", "ken@example.com", 4, "712.00"), ("Barbara Liskov", "barbara@example.com", 4, "540.75"), ("Edsger Dijkstra", "edsger@example.com", 3, "301.20"), ("Margaret Hamilton", "margaret@example.com", 3, "288.00")];
        let query = "-- Paid orders over the last week, by customer\nSELECT c.name, c.email, count(*) AS orders, sum(o.total) AS spent\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'paid'\n  AND o.created_at >= now() - interval '7 days'\nGROUP BY c.id\nORDER BY spent DESC;\n\nUPDATE orders SET status = 'shipped' WHERE id IN (1204326, 1204328);\nSELECT * FROM order_items WHERE order_id = 1204326;";
        let stmt = query.lines().skip(1).take(7).collect::<Vec<_>>().join("\n");
        let first = db::sql::ResultSet {
            cols: vec![col("name", "TEXT", Kind::Text), col("email", "TEXT", Kind::Text), col("orders", "INT8", Kind::Int), col("spent", "NUMERIC", Kind::Decimal)],
            rows: people.iter().filter_map(|&(n, e, o, s)| db::Row::from_doc(&doc! { "name": n, "email": e, "orders": o, "spent": s }).ok()).collect(),
            ms: 31,
            ..db::sql::ResultSet::new(&stmt)
        };
        let refused = db::sql::ResultSet { error: Some("This statement is not allowed on a read-only connection. Unlock writes to run it.".into()), refused: true, ..db::sql::ResultSet::new("UPDATE orders SET status = 'shipped' WHERE id IN (1204326, 1204328)") };
        let last = db::sql::ResultSet {
            cols: vec![col("order_id", "INT8", Kind::Int), col("sku", "TEXT", Kind::Text), col("qty", "INT4", Kind::Int)],
            rows: [("SKU-104", 2_i64), ("SKU-221", 1)].iter().filter_map(|&(sku, q)| db::Row::from_doc(&doc! { "order_id": 1204326_i64, "sku": sku, "qty": q }).ok()).collect(),
            ms: 4,
            ..db::sql::ResultSet::new("SELECT * FROM order_items WHERE order_id = 1204326")
        };
        if let Some(s) = t.sql.as_mut() {
            s.caret = query.lines().take(6).map(|l| l.chars().count() + 1).sum::<usize>() - 1;
            (s.query, s.sets) = (query.into(), vec![first, refused, last]);
        }
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
    }

    /// The Shell of the design: a finished run and its Queries panel, with PostgreSQL's `$1` placeholders.
    fn pg_shell_demo(&mut self) {
        let t = &mut self.tabs[0];
        t.view = View::Shell;
        t.selected = None;
        t.shell.code = "async function run() {\n  const rows = await sql`\n    SELECT customer_id, count(*) AS n FROM orders\n    WHERE status = ${'paid'} GROUP BY customer_id HAVING count(*) > 3`\n  for (const r of rows) {\n    r.last = await orders.findOne({ customer_id: r.customer_id }, { orderBy: '-id' })\n  }\n  return rows\n}".into();
        let rows = [(48120_i64, 9_i64, 1204331_i64, "paid", "112.50"), (9033, 7, 1204330, "shipped", "26.75"), (61, 6, 1204328, "paid", "1,204.10"), (120554, 5, 1204329, "paid", "34.00")];
        let docs: Vec<Bson> = rows.iter().map(|&(c, n, id, st, tot)| Bson::Document(doc! { "customer_id": c, "n": n, "last": { "id": id, "status": st, "total": tot } })).collect();
        t.shell.set_value(Bson::Array(docs));
        t.shell.hide_absent_id();
        t.shell.ms = Some(84);
        t.shell.queries = vec![
            db::ShellQuery { conn: "analytics-prod".into(), text: "SELECT customer_id, count(*) AS n FROM orders WHERE status = $1 GROUP BY customer_id HAVING count(*) > 3".into(), params: "[\"paid\"]".into(), rows: Some(12), affected: None, error: None, ms: 22 },
            db::ShellQuery { conn: "analytics-prod".into(), text: "SELECT * FROM \"public\".\"orders\" WHERE \"customer_id\" = $1 ORDER BY \"id\" DESC LIMIT 1".into(), params: String::new(), rows: Some(1), affected: None, error: None, ms: 5 },
        ];
        t.shell.queries_tab = true;
    }

    /// QUARRY_TEST_PG=acquery: a Query tab with the caret after `c.`, its list opened by the bar preset.
    fn pg_acquery_demo(&mut self) {
        let mut t = Tab::new(Sel { conn: CONN, db: DB.into(), schema: SCHEMA.into(), coll: String::new() }).for_engine(db::sql::Engine::Postgres);
        t.view = View::Sql;
        if let Some(s) = t.sql.as_mut() {
            s.query = "SELECT o.id, c.name FROM orders o\nJOIN customers c ON c.".into();
            s.caret = s.query.chars().count();
            s.bar.preset.ac = true;
        }
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
    }

    /// The connection form on the PostgreSQL card: `connform` on the Standard tab, `paste` with the pasted string filled in
    /// (seeded by the form itself), `tls` on the TLS tab.
    fn pg_form_demo(&mut self, which: &str) {
        let tls = which == "tls";
        self.modal = Some(Modal::Conn(Box::new(ConnForm {
            id: Some(2),
            name: "analytics-prod".into(),
            tag: "Production".into(),
            engine: db::sql::Engine::Postgres,
            addr: db::sql::Addr { scheme: "postgres".into(), host: "analytics-prod.internal".into(), port: 5432, user: "analyst".into(), password: "hunter2hunter".into(), database: "analytics".into(), ssl_mode: if tls { db::sql::SslMode::VerifyIdentity } else { Default::default() }, ssl_ca: if tls { "~/.postgresql/root.crt".into() } else { String::new() } },
            tab: if tls { ConnTab::Tls } else { ConnTab::Standard },
            ..Default::default()
        })));
    }
}

/// The query bar's state for rules, sqlmode, grouped and ac, written in PostgreSQL quoting.
fn pg_bar_demo(which: &str, t: &mut Tab) {
    match which {
        "rules" => {
            t.filter_txt = r#""status" IN ('paid', 'shipped') AND "total" > 50"#.into();
            t.sort_txt = r#""created_at" DESC, "total" ASC"#.into();
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
        "ac" => {
            t.filter_txt = "status = 'paid' AND total > 50 AND cust".into();
            if let Some(s) = t.sql.as_mut() {
                s.bar.preset.ac = true;
                s.bar.c.sql = true;
                s.bar.c.limit = "100".into();
            }
        }
        "grouped" => {
            t.filter_txt = r#""status" IN ('paid', 'shipped') AND "total" > 50"#.into();
            t.sort_txt = r#""revenue" DESC"#.into();
            let col = |name: &str, ty: &str, kind: Kind| Col::plain(name, ty, kind, false);
            let rows: [(&str, i64, &str, &str); 5] = [("paid", 61, "6912.40", "113.32"), ("shipped", 24, "2301.75", "95.91"), ("pending", 22, "1980.00", "90.00"), ("refunded", 14, "1204.10", "86.01"), ("cancelled", 12, "840.25", "70.02")];
            t.docs = rows.iter().filter_map(|&(st, n, rev, avg)| db::Row::from_doc(&doc! { "status": st, "orders": n, "revenue": rev, "avg_total": avg }).ok()).collect();
            t.fields = ["status", "orders", "revenue", "avg_total"].map(String::from).to_vec();
            t.count = 5;
            t.selected = Some((false, 1));
            if let Some(s) = t.sql.as_mut() {
                s.bar.table_cols = std::mem::take(&mut s.cols);
                s.cols = Arc::new(vec![col("status", "order_status", Kind::Enum), col("orders", "INT8", Kind::Int), col("revenue", "NUMERIC", Kind::Decimal), col("avg_total", "NUMERIC", Kind::Decimal)]);
                s.pk.clear();
                s.matched = Some(148);
                s.bar.c.group = r#""status""#.into();
                s.bar.c.select = r#""status", COUNT(*) AS "orders", SUM("total") AS "revenue", AVG("total") AS "avg_total""#.into();
                s.bar.c.having = r#""orders" > 10"#.into();
            }
        }
        _ => {}
    }
}

/// The structure of orders: eleven columns, four indexes (two of them owned by constraints), the primary key, unique and
/// check constraints, a key to customers and the keys that point here, and the audit trigger.
fn orders_structure() -> Structure {
    let col = |name: &str, key: &str, raw: Option<&str>, extra: &str, comment: &str| {
        let &(_, ty, _, cat, typtype, nullable) = ORDERS.iter().find(|c| c.0 == name).expect("a demo column");
        let (default, default_expr) = db::sql::structure::normalize_default(db::sql::Dialect::Postgres, raw.map(Into::into), nullable, extra);
        Column { name: name.into(), full_type: ty.into(), nullable, key: key.into(), default, default_expr, extra: extra.into(), comment: comment.into(), kind: Some(Kind::from_pg(ty.split('(').next().unwrap_or(ty), cat, typtype)), default_raw: raw.map(Into::into), ..Default::default() }
    };
    let ix = |name: &str, unique: bool, cols: &[(&str, bool)], constraint: bool, def: &str| Index { name: name.into(), unique, cols: cols.iter().map(|(n, d)| IndexCol { name: (*n).into(), desc: *d, ..Default::default() }).collect(), kind: "btree".into(), def: def.into(), constraint, ..Default::default() };
    let fk = |name: &str, table: &str, c: &str, rt: &str, rc: &str, del: &str| ForeignKey { name: name.into(), db: SCHEMA.into(), table: table.into(), cols: vec![c.into()], ref_db: SCHEMA.into(), ref_table: rt.into(), ref_cols: vec![rc.into()], on_update: "NO ACTION".into(), on_delete: del.into() };
    let constraint = |name: &str, kind: char, def: &str| Constraint { name: name.into(), kind, def: def.into() };
    let mut st = Structure {
        columns: vec![
            col("id", "PRI", None, "auto_increment GENERATED ALWAYS AS IDENTITY", ""),
            col("customer_id", "MUL", None, "", "FK customers.id"),
            col("public_id", "UNI", Some("gen_random_uuid()"), "", "Shown to customers"),
            col("status", "MUL", Some("'pending'::order_status"), "", ""),
            col("total", "", Some("0.00"), "", "Incl. tax"),
            col("total_cents", "", None, "GENERATED ALWAYS AS (((total * (100)::numeric))::bigint) STORED", ""),
            col("gift", "", Some("false"), "", ""),
            col("tags", "", Some("'{}'::text[]"), "", ""),
            col("meta", "", Some("'{}'::jsonb"), "", ""),
            col("note", "", None, "", ""),
            col("created_at", "MUL", Some("now()"), "", ""),
        ],
        indexes: vec![
            ix("orders_pkey", true, &[("id", false)], true, "CREATE UNIQUE INDEX orders_pkey ON public.orders USING btree (id)"),
            ix("orders_public_id_key", true, &[("public_id", false)], true, "CREATE UNIQUE INDEX orders_public_id_key ON public.orders USING btree (public_id)"),
            ix("idx_orders_status", false, &[("status", false)], false, "CREATE INDEX idx_orders_status ON public.orders USING btree (status)"),
            ix("idx_orders_customer_created", false, &[("customer_id", false), ("created_at", true)], false, "CREATE INDEX idx_orders_customer_created ON public.orders USING btree (customer_id, created_at DESC)"),
        ],
        constraints: vec![constraint("orders_pkey", 'p', "PRIMARY KEY (id)"), constraint("orders_public_id_key", 'u', "UNIQUE (public_id)"), constraint("orders_total_check", 'c', "CHECK ((total >= (0)::numeric))")],
        foreign_keys: vec![fk("orders_customer_id_fkey", "orders", "customer_id", "customers", "id", "RESTRICT")],
        referenced_by: vec![fk("order_items_order_id_fkey", "order_items", "order_id", "orders", "id", "CASCADE"), fk("events_order_id_fkey", "events", "order_id", "orders", "id", "SET NULL")],
        triggers: vec![Trigger { name: "orders_audit".into(), timing: "AFTER".into(), event: "UPDATE".into(), body: "CREATE TRIGGER orders_audit AFTER UPDATE ON public.orders FOR EACH ROW EXECUTE FUNCTION log_order_change()".into(), ..Default::default() }],
        pg_schema: Some(SCHEMA.into()),
        status: db::sql::structure::Status { engine: "heap".into(), rows: Some(ORDERS_ROWS), avg_row: Some(264), data: Some(318_767_104), index: Some(105_906_176), total: Some(424_673_280), comment: "Customer orders".into(), ..Default::default() },
        ..Default::default()
    };
    st.create = db::sql::structure::pg_create_table(SCHEMA, "orders", &st);
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_follow_the_generated_column() {
        assert_eq!([cents("112.50"), cents("9.99"), cents("1204.10"), cents("45")], [11250, 999, 120410, 4500]);
    }

    #[test]
    fn demo_rows_match_their_columns() {
        let cols = orders_cols();
        assert_eq!(cols.len(), ORDERS.len());
        let by = |n: &str| cols.iter().find(|c| c.name == n).map(|c| c.kind);
        assert_eq!([by("id"), by("public_id"), by("status"), by("gift"), by("tags"), by("meta"), by("total"), by("created_at")], [Some(Kind::Int), Some(Kind::Uuid), Some(Kind::Enum), Some(Kind::Bool), Some(Kind::Array), Some(Kind::Json), Some(Kind::Decimal), Some(Kind::DateTime)]);
        for o in &ROWS {
            let d = o.doc();
            assert!(cols.iter().all(|c| d.contains_key(&c.name)), "row {} misses a column", o.id);
        }
        assert_eq!(public_id(1204331).len(), 36);
    }

    #[test]
    fn the_structure_writes_the_create_table() {
        let st = orders_structure();
        assert!(st.create.contains("GENERATED ALWAYS AS IDENTITY") && st.create.contains("STORED") && st.create.contains("CHECK ((total >= (0)::numeric))"), "{}", st.create);
        assert!(st.create.contains("customers"), "{}", st.create);
        assert_eq!(st.indexes.iter().filter(|i| i.constraint).count(), 2);
    }

    #[test]
    fn the_pasted_string_fills_the_form() {
        let a = db::sql::Addr::from_paste("postgresql://analyst@analytics-prod.internal:5432/analytics?sslmode=verify-full").unwrap();
        assert_eq!((a.host.as_str(), a.port, a.user.as_str(), a.database.as_str(), a.ssl_mode), ("analytics-prod.internal", 5432, "analyst", "analytics", db::sql::SslMode::VerifyIdentity));
    }

    #[test]
    fn schema_counts_add_up_to_the_lists() {
        let all = schemas();
        assert_eq!(all.iter().find(|s| s.name == SCHEMA).map(|s| s.tables), Some(names(&TABLES, &VIEWS).len() as u64));
        assert_eq!(super::super::sql_ns::listed_schemas(&all, false).len(), 3);
    }
}
