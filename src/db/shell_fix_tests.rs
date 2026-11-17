//! Shell fixes in the MongoDB runner, through `Worker::shell` as the Shell tab uses it: the logs arrive as `ShellLog` and the result
//! as a value already parsed on the worker in `ShellDone`. The UI tests run their scripts on PostgreSQL, since a MongoDB run reads
//! its address from the keychain, so the parts that need the driver are here. Each test works in its own database of
//! QUARRY_TEST_MONGO and skips when that is unset or Bun is missing. The package install test also needs the network a live run has.

use super::shell::{ShellJob, bun_missing, prepare_shell_dir, runner_package};
use super::{Msg, Res, Worker};
use mongodb::bson::{Bson, Document, doc};
use std::time::Duration;

struct Run {
    logs: Vec<String>,
    res: Res<Bson>,
}

impl Run {
    /// The document the script returned.
    fn doc(self) -> Document {
        match self.res {
            Ok(Bson::Document(d)) => d,
            other => panic!("{other:?} {:?}", self.logs),
        }
    }

    /// The rows the script returned.
    fn rows(self) -> Vec<Bson> {
        match self.res {
            Ok(Bson::Array(a)) => a,
            other => panic!("{other:?} {:?}", self.logs),
        }
    }
}

/// Runs `code` on QUARRY_TEST_MONGO with `db` as its database, writes allowed, as the Shell tab would.
fn run(db: &str, code: &str) -> Option<Run> {
    let uri = std::env::var("QUARRY_TEST_MONGO").ok()?;
    if bun_missing() {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let w = Worker::new(tx, eframe::egui::Context::default());
    let input = serde_json::json!({ "mongo": [{ "name": "test", "uri": uri, "db": db, "proxy": null, "collections": [], "readOnly": false }], "sql": [], "code": code, "packages": [], "timeoutMs": 30000, "main": "test" });
    let job = ShellJob { runtime: "bun".into(), dir: std::env::temp_dir().join("quarry-shell-live-test"), input: input.to_string(), packages: Vec::new(), sql: Vec::new() };
    // Kept until the answer: a dropped sender reads as Stop.
    let (_stop, cancel) = tokio::sync::oneshot::channel();
    w.shell(1, job, cancel);
    let mut logs = Vec::new();
    loop {
        // The first run in a fresh folder installs the packages.
        match rx.recv_timeout(Duration::from_secs(300)).expect("the shell never answered") {
            // "info" lines are the app's notes about the package install, not the script's.
            Msg::ShellLog { level, text, .. } if level != "info" => logs.push(text),
            Msg::ShellDone { res, .. } => return Some(Run { logs, res }),
            _ => {}
        }
    }
}

fn count(db: &str, coll: &str) -> u64 {
    let uri = std::env::var("QUARRY_TEST_MONGO").unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async { mongodb::Client::with_uri_str(&uri).await.unwrap().database(db).collection::<Document>(coll).count_documents(doc! {}).await.unwrap() })
}

#[test]
fn live_shell_fix_returns_an_aggregation_ending_in_out_or_merge() {
    let db = "quarry_shell_fix_agg";
    let seed = "await db.dropDatabase(); await db.collection('src').insertMany([{ _id: 1 }, { _id: 2 }, { _id: 3 }]);";
    // A $limit appended after $out or $merge made the server refuse the pipeline.
    let Some(r) = run(db, &format!("async function run() {{ {seed} return db.collection('src').aggregate([{{ $match: {{}} }}, {{ $out: 'copied' }}]); }}")) else { return };
    assert_eq!(r.res, Ok(Bson::Array(Vec::new())));
    assert_eq!(count(db, "copied"), 3);
    let Some(r) = run(db, "async function run() { return db.collection('src').aggregate([{ $match: { _id: { $gt: 1 } } }, { $merge: { into: 'merged' } }]); }") else { return };
    assert_eq!(r.res, Ok(Bson::Array(Vec::new())));
    assert_eq!(count(db, "merged"), 2);
    // A plain aggregation still gets its limit.
    let Some(r) = run(db, "async function run() { await db.collection('many').insertMany(Array.from({ length: 150 }, (_, i) => ({ i }))); return db.collection('many').aggregate([{ $match: {} }]); }") else { return };
    assert!(matches!(&r.res, Ok(Bson::Array(a)) if a.len() == 100), "{:?}", r.res.map(|v| v.to_string()));
}

#[test]
fn live_shell_fix_returns_the_rows_of_list_collections_and_list_indexes() {
    let db = "quarry_shell_fix_list";
    let Some(r) = run(db, "async function run() { await db.dropDatabase(); await db.collection('alpha').insertOne({ a: 1 }); await db.collection('alpha').createIndex({ a: 1 }); return db.listCollections(); }") else { return };
    let rows = r.rows();
    assert!(rows.iter().any(|d| d.as_document().and_then(|d| d.get_str("name").ok()) == Some("alpha")), "{rows:?}");
    let Some(r) = run(db, "async function run() { return db.collection('alpha').listIndexes(); }") else { return };
    let rows = r.rows();
    let names: Vec<&str> = rows.iter().filter_map(|d| d.as_document()?.get_str("name").ok()).collect();
    assert_eq!(names, ["_id_", "a_1"]);
}

#[test]
fn live_shell_fix_logs_errors_with_their_fields_and_survives_logging_db_and_cursors() {
    let code = "async function run() {
  const e = new Error('boom'); e.code = 42; e.errInfo = { why: 'test' };
  console.log(e);
  try { await db.command({ quarryNoSuchCommand: 1 }) } catch (err) { console.log(err) }
  console.log(db);
  console.log(db.collection('x').find({}));
  return 'done';
}";
    let Some(r) = run("quarry_shell_fix_log", code) else { return };
    // Logging db or a cursor threw on its circular fields and ended the script.
    assert_eq!(r.res, Ok(Bson::String("done".into())), "{:?}", r.logs);
    assert_eq!(r.logs.len(), 4, "{:?}", r.logs);
    let own = &r.logs[0];
    // The message and stack are not enumerable, so they were missing before.
    assert!(own.contains("boom") && own.contains(" at ") && own.contains("42") && own.contains("errInfo") && own.contains("test"), "{own}");
    let server = &r.logs[1];
    assert!(server.contains("quarryNoSuchCommand") && server.contains("code") && server.contains("59"), "{server}");
    assert!(r.logs[2..].iter().all(|l| !l.trim().is_empty()), "{:?}", r.logs);
}

#[test]
fn live_shell_fix_numbers_past_2_63_come_back_as_doubles() {
    let code = "async function run() {
  await db.dropDatabase();
  await db.collection('nums').insertOne({ _id: 1, v: 2 ** 63 });
  const [d] = await db.collection('nums').find({}).toArray();
  return { read: d.v, made: -(2 ** 64), small: 5 };
}";
    let Some(r) = run("quarry_shell_fix_num", code) else { return };
    let d = r.doc();
    assert_eq!(d.get("read"), Some(&Bson::Double(9223372036854775808.0)), "{d}");
    assert_eq!(d.get("made"), Some(&Bson::Double(-18446744073709551616.0)), "{d}");
    assert_eq!(d.get("small"), Some(&Bson::Int32(5)), "{d}");
}

#[test]
fn live_shell_fix_lone_surrogates_with_the_driver_loaded() {
    let code = r#"async function run() {
  console.log('a\uD800b', { k: 'c\uDC00' });
  return { s: 'x\uDC00y', ['k\uD800']: [1, 'z\uD800'] };
}"#;
    let Some(r) = run("quarry_shell_fix_utf", code) else { return };
    let logs = r.logs.clone();
    let d = r.doc();
    assert_eq!(d.get_str("s").ok(), Some("x\u{FFFD}y"), "{d}");
    assert_eq!(d.get_array("k\u{FFFD}").ok().and_then(|a| a.get(1)).and_then(Bson::as_str), Some("z\u{FFFD}"), "{d}");
    assert!(logs.len() == 1 && logs[0].starts_with("a\u{FFFD}b") && logs[0].contains("c\u{FFFD}"), "{logs:?}");
}

/// An install that failed or was stopped must be tried again on the next run. Comparing package.json with what the app would
/// write skipped that retry, since the failed run had already written it, and the old versions kept running.
#[test]
fn live_shell_fix_a_failed_package_install_is_retried() {
    if std::env::var("QUARRY_TEST_MONGO").is_err() || bun_missing() {
        return;
    }
    let dir = std::env::temp_dir().join(format!("quarry-shell-install-retry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let prepare = |spec: &str| rt.block_on(prepare_shell_dir(std::path::Path::new("bun"), &dir, &[spec.to_string()], &|_: &str| {}));
    let marker = || std::fs::read_to_string(dir.join("installed.json")).ok();
    let good = runner_package(&["is-number@7.0.0".to_string()]);
    prepare("is-number@7.0.0").expect("the first install");
    assert_eq!(marker().as_deref(), Some(good.as_str()));
    // No such version: bun fails, while node_modules/is-number stays, so only the marker can tell an install is due.
    assert!(prepare("is-number@999.0.0").is_err());
    assert_eq!(marker().as_deref(), Some(good.as_str()), "a failed install was marked done");
    assert!(dir.join("node_modules").join("is-number").exists());
    assert!(prepare("is-number@999.0.0").is_err(), "the failed install was not tried again");
    prepare("is-number@7.0.0").expect("back to the installed version");
    assert_eq!(marker().as_deref(), Some(good.as_str()));
    let _ = std::fs::remove_dir_all(&dir);
}
