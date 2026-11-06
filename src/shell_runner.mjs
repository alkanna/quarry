// Runs one Shell script for Quarry under Bun, with the official MongoDB driver.
// Input (stdin, JSON): { uri, db, code, collections: [names], readOnly, timeoutMs }
// Output (stdout, one JSON object per line): { type: "log", level, text } ... then
// { type: "result", ok: true, ejson } or { type: "result", ok: false, error }.
import { MongoClient, ObjectId as BsonObjectId, BSON, Collection, Db, Admin, AggregationCursor } from "mongodb";
import { createRequire } from "module";

// Packages added in Quarry are installed next to this file; scripts load them with require() or import().
const require = createRequire(import.meta.url);

const out = (o) => process.stdout.write(JSON.stringify(o) + "\n");
const fmt = (m) => (typeof m === "string" ? m : BSON.EJSON.stringify(m, { relaxed: true }, 2));
const makeLog = (level) => (...m) => out({ type: "log", level, text: m.map(fmt).join(" ") });
const console2 = { log: makeLog("log"), info: makeLog("log"), warn: makeLog("warn"), error: makeLog("error") };

const input = JSON.parse(await Bun.stdin.text());

if (input.readOnly) {
  // The connection is locked: refuse anything that writes, whatever the script does.
  const blocked = (name) => function () {
    throw new Error(`${name}() is a write and this connection is read-only. Unlock writes in Quarry first.`);
  };
  for (const m of ["insertOne", "insertMany", "updateOne", "updateMany", "replaceOne", "deleteOne", "deleteMany",
    "bulkWrite", "findOneAndUpdate", "findOneAndReplace", "findOneAndDelete", "drop", "rename", "createIndex",
    "createIndexes", "dropIndex", "dropIndexes", "createSearchIndex", "createSearchIndexes", "dropSearchIndex",
    "updateSearchIndex", "initializeOrderedBulkOp", "initializeUnorderedBulkOp"]) {
    Collection.prototype[m] = blocked(m);
  }
  for (const m of ["dropDatabase", "dropCollection", "createCollection", "renameCollection", "createIndex",
    "removeUser", "setProfilingLevel"]) {
    Db.prototype[m] = blocked(m);
  }
  Admin.prototype.removeUser = blocked("removeUser");
  // Scripts reach the client through db.client, and its bulkWrite spans collections.
  MongoClient.prototype.bulkWrite = blocked("bulkWrite");
  const READ_COMMANDS = new Set(["ping", "buildinfo", "buildInfo", "serverStatus", "dbStats", "dbstats", "collStats",
    "listCollections", "listIndexes", "count", "distinct", "find", "hello", "isMaster", "explain", "connectionStatus",
    "replSetGetStatus", "listDatabases"]);
  // Admin.command and runCursorCommand send commands without going through Db.command.
  const guardCommand = (proto, method, label) => {
    const run = proto[method];
    proto[method] = function (cmd, ...rest) {
      const name = Object.keys(cmd || {})[0];
      if (!READ_COMMANDS.has(name)) throw new Error(`${label}({ ${name} }) is not allowed on a read-only connection.`);
      return run.call(this, cmd, ...rest);
    };
  };
  guardCommand(Db.prototype, "command", "db.command");
  guardCommand(Db.prototype, "runCursorCommand", "db.runCursorCommand");
  guardCommand(Admin.prototype, "command", "db.admin().command");
  const writesData = (pipeline) => (pipeline || []).some((s) => s && ("$out" in s || "$merge" in s));
  const PIPELINE_WRITE = "$out and $merge write data; this connection is read-only.";
  const aggregate = Collection.prototype.aggregate;
  Collection.prototype.aggregate = function (pipeline, ...rest) {
    if (writesData(pipeline)) throw new Error(PIPELINE_WRITE);
    return aggregate.call(this, pipeline, ...rest);
  };
  const addStage = AggregationCursor.prototype.addStage;
  AggregationCursor.prototype.addStage = function (stage) {
    if (writesData([stage])) throw new Error(PIPELINE_WRITE);
    return addStage.call(this, stage);
  };
  // The cursor keeps the caller's pipeline array, which can still change before it runs.
  const initialize = AggregationCursor.prototype._initialize;
  AggregationCursor.prototype._initialize = async function (...args) {
    if (writesData(this.pipeline)) throw new Error(PIPELINE_WRITE);
    return initialize.apply(this, args);
  };
}

function packageIdent(name) {
  const base = name.replace(/^@[^/]+\//, "");
  let id = base.split(/[^A-Za-z0-9]+/).filter(Boolean).map((p, i) => (i ? p[0].toUpperCase() + p.slice(1) : p)).join("");
  if (/^[0-9]/.test(id)) id = "_" + id;
  return id || "pkg";
}

// Reserved words are not valid parameter names for new Function, so those collections are only
// reachable through db.collection(). Must match JS_RESERVED in app.rs.
const RESERVED = new Set(["break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
  "else", "enum", "export", "extends", "false", "finally", "for", "function", "if", "import", "in", "instanceof", "new",
  "null", "return", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with",
  "implements", "interface", "let", "package", "private", "protected", "public", "static", "yield", "await",
  "arguments", "eval"]);

// Shell helpers like the original's: ObjectId works with or without `new`.
function ObjectId(...a) { return new BsonObjectId(...a); }
function ISODate(s) { return s ? new Date(s) : new Date(); }
// The original's randomId alphabet: digits and letters of both cases.
const ALPHANUM = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
const randomId = (n = 17) => [...crypto.getRandomValues(new Uint8Array(n))].map((b) => ALPHANUM[b % 62]).join("");
const UUIDv4 = () => crypto.randomUUID();

// SSH-only servers are reached through Quarry's tunnel (a local SOCKS5 port) or the connection's
// own proxy; the driver needs the "socks" package for that, installed as a built-in.
const clientOptions = { appName: "quarry-shell", serverSelectionTimeoutMS: input.timeoutMs };
if (input.proxy) {
  clientOptions.proxyHost = input.proxy.host;
  clientOptions.proxyPort = input.proxy.port;
  if (input.proxy.user) {
    clientOptions.proxyUsername = input.proxy.user;
    clientOptions.proxyPassword = input.proxy.password || "";
  }
}
const client = new MongoClient(input.uri, clientOptions);
try {
  // The driver connects on the first database call, so plain scripts run without a server.
  const db = client.db(input.db);
  const ctx = { db, ObjectId, ISODate, randomId, UUIDv4, require, console: console2, log: console2.log };
  for (const name of input.collections) {
    if (/^[A-Za-z_$][\w$]*$/.test(name) && !RESERVED.has(name) && !(name in ctx)) ctx[name] = db.collection(name);
  }
  // Built in, like the original shell: _ (lodash), dayjs and UUIDv1.
  try {
    ctx._ = require("lodash");
    ctx.dayjs = require("dayjs");
    ctx.UUIDv1 = require("uuid").v1;
  } catch (e) {
    console2.warn(`Could not load the built-in helpers: ${e && e.message ? e.message : e}`);
  }
  // Packages added in Quarry are ready by name: "date-fns" as dateFns, "@faker-js/faker" as faker,
  // and lodash also as _ (like the original shell). Must match package_ident() in app.rs.
  for (const pkg of input.packages || []) {
    const id = packageIdent(pkg);
    try {
      let mod;
      try {
        mod = require(pkg);
      } catch {
        mod = await import(pkg);
      }
      // ES modules come back as a namespace; use their default export when there is one.
      const value = mod && typeof mod === "object" && mod.default !== undefined ? mod.default : mod;
      if (!(id in ctx)) ctx[id] = value;
      if (pkg === "lodash" && !("_" in ctx)) ctx._ = value;
    } catch (e) {
      console2.warn(`Could not load package ${pkg}: ${e && e.message ? e.message : e}`);
    }
  }
  const fn = new Function(...Object.keys(ctx), `return (${input.code.trim()})`)(...Object.values(ctx));
  if (typeof fn !== "function") throw new Error("The command is not a function. Write it as: async function run() { ... }");
  let result = await fn();
  if (result === null) throw new Error("No results");
  if (result === undefined) throw new Error('No results. Did you forget to "return" something in the function?');
  if (result && typeof result.toArray === "function" && typeof result.limit === "function") {
    result = await result.limit(100).toArray();
  }
  out({ type: "result", ok: true, ejson: BSON.EJSON.stringify({ v: result }, { relaxed: false }) });
} catch (e) {
  out({ type: "result", ok: false, error: String(e && e.message ? e.message : e) });
} finally {
  await client.close().catch(() => {});
}
