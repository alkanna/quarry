//! What the live tests of the import and dump code share: a connected worker, and short helpers to run
//! statements on a plain pool. Needs QUARRY_TEST_MYSQL and QUARRY_TEST_MARIADB; a test skips a server whose
//! variable is unset.

use super::csv::TableInfo;
use super::Db;
use crate::db::{Msg, Worker};

pub const ENVS: [&str; 2] = ["QUARRY_TEST_MYSQL", "QUARRY_TEST_MARIADB"];

pub struct Live {
    pub w: Worker,
    pub rx: std::sync::mpsc::Receiver<Msg>,
    pub db: Db,
    pub pool: sqlx::MySqlPool,
}

pub fn live(var: &str) -> Option<Live> {
    let uri = std::env::var(var).ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    let pool = w.rt.block_on(async { sqlx::MySqlPool::connect(&uri).await.unwrap() });
    w.sql_connect(1, uri.clone(), std::time::Duration::from_secs(30), crate::db::Net::default());
    let Msg::SqlConnected { res: Ok((db, _, _)), .. } = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!("connect") };
    Some(Live { w, rx, db, pool })
}

pub fn every_server(f: impl Fn(&Live, &str)) {
    for var in ENVS {
        if let Some(l) = live(var) {
            f(&l, var);
        }
    }
}

impl Live {
    pub fn sql(&self, s: &str) {
        self.w.rt.block_on(async { sqlx::raw_sql(s).execute(&self.pool).await.unwrap_or_else(|x| panic!("{s}: {x}")) });
    }

    /// The first cell of the first row, as text.
    pub fn one(&self, s: &str) -> String {
        self.w.rt.block_on(async {
            let rows = sqlx::raw_sql(s).fetch_all(&self.pool).await.unwrap_or_else(|x| panic!("{s}: {x}"));
            super::worker::text_at(&rows[0], 0).unwrap_or_default()
        })
    }

    /// One cell of the first row, as text.
    pub fn cell(&self, s: &str, col: usize) -> String {
        self.w.rt.block_on(async {
            let rows = sqlx::raw_sql(s).fetch_all(&self.pool).await.unwrap_or_else(|x| panic!("{s}: {x}"));
            super::worker::text_at(&rows[0], col).unwrap_or_default()
        })
    }

    pub fn count(&self, table: &str) -> i64 {
        self.one(&format!("SELECT COUNT(*) FROM {table}")).parse().unwrap()
    }

    pub fn info(&self, schema: &str, table: &str) -> TableInfo {
        self.w.rt.block_on(async { TableInfo::from_structure(&super::structure::load(&self.db, schema, table).await.unwrap()) })
    }

    /// The same server, with every connection of its pool running `set` first: a session zone or sql_mode as a
    /// server configured that way would give it, without a SET GLOBAL that the tests running alongside would see.
    pub fn with_session(&self, set: &str) -> Db {
        with_session(&self.w, &self.db, set)
    }
}

/// `Live::with_session` for tests that keep a worker and a `Db` of their own.
pub fn with_session(w: &Worker, db: &Db, set: &str) -> Db {
    let set = set.to_string();
    let pool = w.rt.block_on(async {
        sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(2)
            .after_connect(move |c, _| {
                let set = set.clone();
                Box::pin(async move { sqlx::Executor::execute(c, set.as_str()).await.map(|_| ()) })
            })
            .connect_with(db.opts.my().unwrap().clone())
            .await
            .unwrap()
    });
    Db { pool: super::Pool::My(pool), ..db.clone() }
}

/// Postgres 18 and 13, the two servers the Postgres code is tested on. A test skips a server whose variable is unset.
pub const PG_ENVS: [&str; 2] = ["QUARRY_TEST_POSTGRES", "QUARRY_TEST_POSTGRES13"];

pub struct LivePg {
    pub w: Worker,
    pub rx: std::sync::mpsc::Receiver<Msg>,
    pub db: Db,
    /// The databases the login can connect to, as the connect answer listed them.
    pub dbs: Vec<String>,
    /// A plain pool of the URI's own database, outside the session guard, for fixtures.
    pub pool: sqlx::PgPool,
    pub uri: String,
}

pub fn live_pg(var: &str) -> Option<LivePg> {
    let uri = std::env::var(var).ok()?;
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    let pool = w.rt.block_on(async { sqlx::PgPool::connect(&uri).await.unwrap() });
    w.sql_connect(1, uri.clone(), std::time::Duration::from_secs(30), crate::db::Net::default());
    let Msg::SqlConnected { res: Ok((db, dbs, _)), .. } = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap() else { panic!("connect") };
    Some(LivePg { w, rx, db, dbs, pool, uri })
}

pub fn every_pg(f: impl Fn(&LivePg, &str)) {
    for var in PG_ENVS {
        if let Some(l) = live_pg(var) {
            f(&l, var);
        }
    }
}

impl LivePg {
    /// The first cell of the first row, as text.
    pub fn one(&self, s: &str) -> String {
        self.w.rt.block_on(async {
            let rows = sqlx::raw_sql(s).fetch_all(&self.pool).await.unwrap_or_else(|x| panic!("{s}: {x}"));
            super::worker::text_at(&rows[0], 0).unwrap_or_default()
        })
    }

    /// A plain pool of the shared `quarry_test` database, outside the session guard, for fixtures. Call `quarry_test`
    /// first so the database exists.
    pub fn fixture_pool(&self) -> sqlx::PgPool {
        use std::str::FromStr;
        let o = sqlx::postgres::PgConnectOptions::from_str(&self.uri).unwrap().database("quarry_test");
        self.w.rt.block_on(async { sqlx::postgres::PgPoolOptions::new().max_connections(2).connect_with(o).await.unwrap() })
    }

    /// Runs statements (several are fine) in `quarry_test` as the fixture login.
    pub fn qsql(&self, pool: &sqlx::PgPool, s: &str) {
        self.w.rt.block_on(async { sqlx::raw_sql(s).execute(pool).await.unwrap_or_else(|x| panic!("{s}: {x}")) });
    }

    /// The first cell of the first row of a query in `quarry_test`, as text.
    pub fn qone(&self, pool: &sqlx::PgPool, s: &str) -> String {
        self.w.rt.block_on(async {
            let rows = sqlx::raw_sql(s).fetch_all(pool).await.unwrap_or_else(|x| panic!("{s}: {x}"));
            super::worker::text_at(&rows[0], 0).unwrap_or_default()
        })
    }

    /// The next message the worker sends.
    pub fn next(&self) -> Msg {
        self.rx.recv_timeout(std::time::Duration::from_secs(20)).expect("worker answer")
    }

    /// The shared database of the live tests, made on first use. Tests of one run take turns making it, because
    /// two CREATE DATABASE at once fail on template1 being in use.
    pub fn quarry_test(&self) -> Db {
        static MAKE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = MAKE.lock().unwrap_or_else(|p| p.into_inner());
        if self.one("SELECT count(*)::text FROM pg_database WHERE datname = 'quarry_test'") == "0" {
            let made = self.w.rt.block_on(async { sqlx::raw_sql("CREATE DATABASE quarry_test").execute(&self.pool).await });
            if let Err(x) = made {
                let code = x.as_database_error().and_then(|d| d.code().map(|c| c.to_string()));
                assert!(matches!(code.as_deref(), Some("42P04" | "23505")), "{x}");
            }
        }
        self.db.at("quarry_test")
    }
}
