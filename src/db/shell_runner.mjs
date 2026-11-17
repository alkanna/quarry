// Runs one Shell script for Quarry under Bun. One runner serves both engines: the script gets the
// tab's own connection ("main") and any attached ones, each under a JS name.
// stdin (JSON lines): the first line is
//   { code, packages, timeoutMs, maxBytes?, main: "<name>",
//     mongo: [{ name, uri, db, proxy, collections, readOnly }], sql: [{ name, db, tables, dialect }] }
// maxBytes turns on document frames (see sendDocs), with that budget; without it every result is one text line.
// and after it come SQL answers { type: "sqlres", id, ok, rows | affected, insertId | error }.
// Mongo entries are MongoClients inside this process. SQL entries never get a driver or a password:
// every query goes to Quarry, which runs it on that connection with its read-only rules. dialect is "postgres"
// for a PostgreSQL entry (placeholders $1..$n, "double quoted" names, ILIKE, RETURNING); any other value is MySQL.
// stdout (JSON lines): { type: "log", level, text }, { type: "sql", conn, id, text, params } per SQL query,
//   { type: "query", conn, text, rows?, affected?, ms, error? } per Mongo command, then either
//   { type: "result", ok: true, ejson }, or document frames, perhaps a { type: "reset" } dropping them, and
//   { type: "result", ok: true, docs, single, cut }; or { type: "result", ok: false, error }.
import { createRequire } from "module";

// Packages added in Quarry are installed next to this file; scripts load them with require() or import().
const require = createRequire(import.meta.url);

// JSON.stringify writes a lone surrogate as an escape serde_json refuses, which would lose the whole line. Keys here
// are fixed names, so the values are all there is to mend; SQL values are refused in wire() instead of changed.
const wellFormed = (_, v) => (typeof v === "string" ? v.toWellFormed() : v);
const write = (o) => new Promise((r) => process.stdout.write(JSON.stringify(o, wellFormed) + "\n", r));
const out = (o) => void write(o);

// The app stops reading a line at LINE_MAX (src/db/shell.rs), so a bigger result is refused here, with its size and a way out.
const LINE_MAX = 64 << 20;
const tooBig = (bytes) =>
  new Error(`The result is about ${Math.ceil(bytes / (1 << 20))} MB, more than the ${LINE_MAX >> 20} MB the Shell can show. Return fewer documents, or only the fields you need with a projection.`);

// The driver is loaded only when an entry needs it, so a SQL-only run starts without it.
let mongodb = null;

// An invalid Date or one past what BSON and ISO parsing accept (years 0 to 9999) would make Quarry refuse the whole result.
const okDate = (v) => {
  const y = v.getUTCFullYear();
  return Number.isNaN(y) || y < 0 || y > 9999 ? null : v;
};

// Rows hold bigints and bytes that JSON.stringify would refuse or spell out as an object of numbers.
// bigint says how a BigInt, or a number at or past 2^63, is spelled: Extended JSON for results, plain for logs.
const walk = (v, bigint) => {
  if (typeof v === "bigint") return bigint(v);
  // js-bson writes 2^63 as a $numberLong no int64 holds, and Quarry would refuse the whole result.
  if (typeof v === "number" && Number.isInteger(v) && Math.abs(v) >= 2 ** 63) return bigint(v);
  if (typeof v === "string") return v.toWellFormed();
  if (v instanceof Uint8Array) return { $binary: { base64: Buffer.from(v).toString("base64"), subType: "00" } };
  if (v instanceof Date) return okDate(v);
  if (Array.isArray(v)) return v.map((x) => walk(x, bigint));
  // Only plain objects are copied: BSON values, dates and class instances keep their own serialization.
  if (v && typeof v === "object" && (Object.getPrototypeOf(v) === Object.prototype || Object.getPrototypeOf(v) === null)) {
    const o = {};
    // defineProperty, because assigning "__proto__" would change the prototype and drop the key.
    for (const k of Object.keys(v)) Object.defineProperty(o, k.toWellFormed(), { value: walk(v[k], bigint), enumerable: true, writable: true, configurable: true });
    return o;
  }
  return v;
};
// The same value prepared for BSON.serialize, which writes bigints, bytes and Long itself. It drops what JSON drops
// (undefined, functions, symbols) so a document reads the same as on the Extended JSON path.
const skipped = (x) => x === undefined || typeof x === "function" || typeof x === "symbol";
const toBson = (v, BSON) => {
  if (typeof v === "bigint") return v >= -(2n ** 63n) && v < 2n ** 63n ? BSON.Long.fromBigInt(v) : v.toString();
  if (typeof v === "string") return v.toWellFormed();
  if (typeof v === "number") {
    // js-bson sizes -0 as a double and then writes it as an int32, and fails.
    if (Object.is(v, -0)) return new BSON.Double(-0);
    // js-bson writes an integer past int32 as a double; the text path gave an int64, which SQL ids and timestamps need.
    if (Number.isInteger(v) && (v < -(2 ** 31) || v >= 2 ** 31) && Math.abs(v) < 2 ** 63) return BSON.Long.fromNumber(v);
    return v;
  }
  if (v instanceof Date) return okDate(v);
  if (!v || typeof v !== "object" || v instanceof Uint8Array || v instanceof RegExp || v._bsontype) return v;
  if (Array.isArray(v)) return v.map((x) => (skipped(x) ? null : toBson(x, BSON)));
  // Extended JSON writes a Map's entries and refuses keys that are not strings; plain JSON writes {}.
  if (v instanceof Map && mongodb) {
    if ([...v.keys()].some((k) => typeof k !== "string")) throw new Error("Can only serialize maps with string keys");
    return toBson(Object.fromEntries(v), BSON);
  }
  // A class with toJSON, such as a dayjs value, reads as what it gives, as JSON.stringify does.
  const proto = Object.getPrototypeOf(v);
  if (proto !== Object.prototype && proto !== null && typeof v.toJSON === "function") {
    const j = v.toJSON();
    return skipped(j) ? null : toBson(j, BSON);
  }
  const o = {};
  for (const k of Object.keys(v)) {
    // Extended JSON, used once the driver is loaded, writes an undefined field as null; plain JSON leaves it out.
    const x = v[k] === undefined && mongodb ? null : v[k];
    if (!skipped(x)) Object.defineProperty(o, k.toWellFormed(), { value: toBson(x, BSON), enumerable: true, writable: true, configurable: true });
  }
  return o;
};
const asLong = (v) => (typeof v === "number" ? { $numberDouble: String(v) } : v >= -(2n ** 63n) && v < 2n ** 63n ? { $numberLong: v.toString() } : v.toString());
const asLog = (v) => (typeof v === "number" || Number.isSafeInteger(Number(v)) ? Number(v) : v.toString());
const serialize = (v) => {
  const w = walk(v, asLong);
  if (mongodb) return mongodb.BSON.EJSON.stringify(w, { relaxed: false });
  // Without the driver only JSON.stringify is left, which would flatten a Date to a bare string.
  return JSON.stringify(w, function (k, x) {
    const raw = this[k];
    return raw instanceof Date ? { $date: Number.isNaN(raw.getTime()) ? null : raw.toISOString() } : x;
  });
};
// What the Extended JSON path would turn into a document, so the app keeps it as a row either way.
const isDoc = (x) => x !== null && typeof x === "object" && !Array.isArray(x) && !(x instanceof Date) && !(x instanceof Uint8Array) && !(x instanceof RegExp) && !x._bsontype;

// SQL-only runs never load the driver, so BSON comes from its own package, which the driver brings along.
const loadBson = async () => {
  try {
    return await import("bson");
  } catch {
    return (await import("mongodb")).BSON;
  }
};

// Documents cross as frames: a {"type":"doc"} line, then the document's BSON, which starts with its own length.
// A text line would be Extended JSON escaped twice, several times the BSON, built whole before the first byte goes out.
// Stops before input.maxBytes and says so. A returned cursor is read batch by batch, so Bun never holds all of it, and is
// closed at the budget, so the server stops sending. Null when nothing was sent, for the text path to take over.
const DOC_HEAD = '{"type":"doc"}\n';
// js-bson serializes into an internal buffer of 17 MB and copies out; a bigger document is cut short without an error.
let bsonRoom = 17 << 20;
// `src` is a returned cursor, a list of documents or one document.
const sendDocs = async (src) => {
  const isCursor = typeof src.toArray === "function";
  const single = !isCursor && !Array.isArray(src);
  if (single) src = [src];
  const BSON = mongodb ? mongodb.BSON : await loadBson();
  let bytes = 0;
  let n = 0;
  // The size of the first document that did not fit, once one did not.
  let cut = 0;
  for await (const d of src) {
    let frame;
    try {
      const w = toBson(d, BSON);
      const size = BSON.calculateObjectSize(w);
      if (bytes + size > input.maxBytes) {
        cut = size;
        break;
      }
      if (size > bsonRoom) BSON.setInternalBufferSize((bsonRoom = size));
      const buf = Buffer.allocUnsafe(DOC_HEAD.length + size);
      buf.write(DOC_HEAD);
      BSON.serializeWithBufferAndIndex(w, buf, { index: DOC_HEAD.length });
      // calculateObjectSize can count more than serialize writes (a Binary of subtype 4 does), so send what was written.
      const len = buf.readInt32LE(DOC_HEAD.length);
      frame = buf.subarray(0, DOC_HEAD.length + len);
      bytes += len;
    } catch (e) {
      if (isCursor) throw e;
      // A key BSON cannot hold, such as one with a NUL: the app drops what it got and the result goes as text, as before.
      if (n) await write({ type: "reset" });
      return null;
    }
    await new Promise((r) => process.stdout.write(frame, r));
    n++;
  }
  if (isCursor && cut) await src.close().catch(() => {});
  // Nothing fit: an error says so, where an empty list would read as no result.
  if (cut && !n) throw new Error(`The first document is about ${Math.ceil(cut / (1 << 20))} MB, more than the ${input.maxBytes >> 20} MB the Shell keeps.`);
  return n ? { docs: n, single, cut: !!cut } : null;
};

// Anything else goes as one Extended JSON line, refused past LINE_MAX.
const sendText = async (result, queryMs) => {
  // Extended JSON is never smaller than BSON, so a result already too big as BSON is refused before it is turned into text.
  let size = 0;
  try {
    if (mongodb) size = mongodb.BSON.calculateObjectSize({ v: result });
  } catch {}
  if (size >= LINE_MAX) throw tooBig(size);
  // The line is measured, not the Extended JSON alone: quoting it inside the line escapes every quote again.
  const line = JSON.stringify({ type: "result", ok: true, ejson: serialize({ v: result }), queryMs });
  const bytes = Buffer.byteLength(line);
  if (bytes >= LINE_MAX) throw tooBig(bytes);
  await new Promise((r) => process.stdout.write(line + "\n", r));
};

const fmt = (m) => {
  if (typeof m === "string") return m;
  // An Error's message and stack are not enumerable, so stringify would print {}. Driver errors keep their details,
  // such as code and errInfo, in enumerable fields.
  if (m instanceof Error) {
    const s = m.stack || String(m);
    const own = Object.fromEntries(Object.entries(m));
    return Object.keys(own).length ? `${s}\n${fmt(own)}` : s;
  }
  try {
    const w = walk(m, asLog);
    return (mongodb ? mongodb.BSON.EJSON.stringify(w, { relaxed: true }, 2) : JSON.stringify(w, null, 2)) ?? String(m);
  } catch {
    // A circular value such as db or a cursor would otherwise end the script. depth keeps a client's internals short.
    return Bun.inspect(m, { depth: 2 });
  }
};
const makeLog = (level) => (...m) => out({ type: "log", level, text: m.map(fmt).join(" ") });
const console2 = { log: makeLog("log"), info: makeLog("log"), warn: makeLog("warn"), error: makeLog("error") };

// Answers arrive on stdin while the script runs, so they are read in the background.
const pending = new Map();
let nextId = 0;
let onInput;
const inputLine = new Promise((r) => (onInput = r));
let first = true;
(async () => {
  const dec = new TextDecoder();
  let buf = "";
  const take = (line) => {
    if (!line.trim()) return;
    if (first) {
      first = false;
      return onInput(JSON.parse(line));
    }
    // Bytes and integers beyond 2^53 come back as Extended JSON, which a plain number could not hold.
    const m = JSON.parse(line, (_, v) => {
      if (v && typeof v === "object") {
        // A SQL column can be named $binary or $numberLong. Its value then stays as it came.
        try {
          if (v.$binary) return Buffer.from(v.$binary.base64, "base64");
          if (typeof v.$numberLong === "string") return BigInt(v.$numberLong);
        } catch {}
      }
      return v;
    });
    const p = pending.get(m.id);
    if (!p) return;
    pending.delete(m.id);
    if (!m.ok) return p.reject(new Error(m.error));
    const rows = m.rows ?? [];
    rows.affected = m.affected ?? 0;
    // PostgreSQL reports a key only for an INSERT ... RETURNING of one row (its first column), so otherwise it stays undefined.
    rows.insertId = m.insertId ?? (p.pg ? undefined : 0);
    p.resolve(rows);
  };
  for await (const chunk of Bun.stdin.stream()) {
    // Search only the new part: the old part has no newline, and rescanning it is quadratic for a big answer.
    let from = buf.length;
    buf += dec.decode(chunk, { stream: true });
    let start = 0;
    let i;
    while ((i = buf.indexOf("\n", from)) >= 0) {
      take(buf.slice(start, i));
      start = from = i + 1;
    }
    buf = buf.slice(start);
  }
  take(buf);
  for (const p of pending.values()) p.reject(new Error("Quarry closed the connection"));
  pending.clear();
})();

// Half of a UTF-16 surrogate pair, often from cutting a string in the middle of an emoji. Written out it would become a
// replacement character, storing other text than the script holds, so a query holding one is refused.
const broken = (v) =>
  typeof v === "string" ? !v.isWellFormed()
  : Array.isArray(v) ? v.some(broken)
  : v !== null && typeof v === "object" && Object.getPrototypeOf(v) === Object.prototype ? Object.entries(v).some(([k, x]) => !k.isWellFormed() || broken(x))
  : false;

// What a value becomes on the wire. Quarry turns each one into a literal; nothing here is spliced into text.
function wire(v, pg) {
  if (v === undefined || v === null) return null;
  // PostgreSQL has a boolean type that 1 does not satisfy.
  if (typeof v === "boolean") return pg ? { $bool: v } : v ? 1 : 0;
  if (typeof v === "bigint") return v >= -9007199254740991n && v <= 9007199254740991n ? Number(v) : { $int: v.toString() };
  if (typeof v === "number") {
    if (!Number.isFinite(v)) throw new Error("A query value is not a finite number");
    // Past 2^53 the number is already rounded, so the digits the script meant are gone. Use a BigInt.
    if (Number.isInteger(v) && !Number.isSafeInteger(v)) throw new Error("A query value is an integer beyond 2^53 that a number cannot hold. Pass it as a BigInt, like 123n");
    return v;
  }
  if (v instanceof Date) {
    if (Number.isNaN(v.getTime())) throw new Error("A query value is an invalid Date");
    return pg ? v.toISOString() : v.toISOString().replace("T", " ").replace("Z", "");
  }
  if (v instanceof Uint8Array) return { $hex: Buffer.from(v).toString("hex") };
  if (typeof v === "object") return JSON.stringify(walk(v, asLog));
  return v;
}

function query(conn, text, params) {
  const id = ++nextId;
  const pg = dialects.get(conn) === "postgres";
  let wired;
  try {
    // One check over everything the query sends, before any of it is turned into text.
    if (broken([text, ...params])) throw new Error("The query holds half of a UTF-16 surrogate pair, often from cutting a string in the middle of an emoji");
    wired = params.map((v) => wire(v, pg));
  } catch (e) {
    return Promise.reject(e);
  }
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject, pg });
    out({ type: "sql", conn, id, text, params: wired });
  });
}

// sql`... ${v} ...` becomes text with ? placeholders, or $1..$n on PostgreSQL where ? is a jsonb operator.
// An array expands to one placeholder per item, for IN (...).
function makeSql(conn) {
  const pg = dialects.get(conn) === "postgres";
  const sql = (strings, ...values) => {
    let text = strings[0];
    const params = [];
    const mark = (v) => {
      params.push(v);
      return pg ? "$" + params.length : "?";
    };
    values.forEach((v, i) => {
      text += Array.isArray(v) ? (v.length ? v.map(mark).join(", ") : "NULL") : mark(v);
      // A digit right after $n would read as part of the number ($1 then 0 is $10).
      if (pg && /^\d/.test(strings[i + 1])) text += " ";
      text += strings[i + 1];
    });
    return query(conn, text, params);
  };
  sql.unsafe = (text, params = []) => query(conn, String(text), params);
  // On PostgreSQL a second argument names the schema: sql.table("orders", "shop") is "shop"."orders".
  sql.table = (name, schema) => makeTable(conn, name, schema);
  // An attached connection is the tag itself (other`...`); .sql also works so it reads like the main connection's sql`...`.
  sql.sql = sql;
  return sql;
}

// What differs per dialect in the SQL a table helper builds. MySQL's text is the reference and must not change.
const MYSQL = { ident: (n) => "`" + String(n).replaceAll("`", "``") + "`", mark: () => "?", like: "LIKE", all: "18446744073709551615" };
// ILIKE because MySQL's LIKE ignores case under the default collations, so a script reads the same on both.
const POSTGRES = { ident: (n) => '"' + String(n).replaceAll('"', '""') + '"', mark: (params) => "$" + params.length, like: "ILIKE", all: "ALL" };
const dialects = new Map();
const dialectOf = (conn) => (dialects.get(conn) === "postgres" ? POSTGRES : MYSQL);

const isPlain = (v) => v !== null && typeof v === "object" && (Object.getPrototypeOf(v) === Object.prototype || Object.getPrototypeOf(v) === null);

// where: { col: value } is =, null is IS NULL, an array is IN, and { gt, gte, lt, lte, ne, like, in } compare.
// Anything unclear throws: a condition that silently vanishes would turn an update or delete into every row.
function compileWhere(where, params, d) {
  const { ident, mark } = d;
  if (where === undefined) return "";
  if (!isPlain(where)) throw new Error("where must be an object like { id: 1 }. Pass {} for every row.");
  const parts = [];
  const eq = (c, v) => {
    if (v === undefined) throw new Error(`The value for "${c}" is undefined. Use null to match NULL.`);
    if (v === null) return `${ident(c)} IS NULL`;
    if (Array.isArray(v)) {
      if (v.includes(undefined)) throw new Error(`A list for "${c}" holds undefined`);
      return v.length ? `${ident(c)} IN (${v.map((x) => (params.push(x), mark(params))).join(", ")})` : "FALSE";
    }
    if (typeof v === "object" && !(v instanceof Date || v instanceof Uint8Array)) throw new Error(`The value for "${c}" is not a plain value, a list or a condition object`);
    params.push(v);
    return `${ident(c)} = ${mark(params)}`;
  };
  const ops = { gt: ">", gte: ">=", lt: "<", lte: "<=", like: d.like };
  for (const [c, v] of Object.entries(where)) {
    if (!isPlain(v)) {
      parts.push(eq(c, v));
      continue;
    }
    const entries = Object.entries(v);
    if (!entries.length) throw new Error(`The condition for "${c}" is empty. Use gt, gte, lt, lte, ne, like or in.`);
    for (const [op, x] of entries) {
      if (x === undefined) throw new Error(`The "${op}" value for "${c}" is undefined`);
      if (op in ops) {
        params.push(x);
        parts.push(`${ident(c)} ${ops[op]} ${mark(params)}`);
      } else if (op === "ne") {
        if (x !== null) params.push(x);
        parts.push(x === null ? `${ident(c)} IS NOT NULL` : `${ident(c)} <> ${mark(params)}`);
      } else if (op === "in") {
        parts.push(eq(c, x ?? []));
      } else {
        throw new Error(`Unknown condition "${op}" on ${c}. Use gt, gte, lt, lte, ne, like or in.`);
      }
    }
  }
  return parts.length ? " WHERE " + parts.join(" AND ") : "";
}

const whole = (n, what) => {
  if (!Number.isInteger(n) || n < 0) throw new Error(`${what} must be a whole number`);
  return n;
};

// orderBy: "col", "-col" for descending, { col: "desc" }, or a list of those.
function compileOrder(o, { ident }) {
  if (o === undefined) return "";
  const items = [];
  for (const x of Array.isArray(o) ? o : [o]) {
    if (typeof x === "string") items.push(x.startsWith("-") ? `${ident(x.slice(1))} DESC` : ident(x));
    else for (const [c, d] of Object.entries(x)) items.push(`${ident(c)} ${String(d).toLowerCase() === "desc" ? "DESC" : "ASC"}`);
  }
  return items.length ? " ORDER BY " + items.join(", ") : "";
}

function makeTable(conn, name, schema) {
  const d = dialectOf(conn);
  const { ident, mark } = d;
  const pg = d === POSTGRES;
  const t = pg && schema !== undefined && schema !== null ? `${ident(schema)}.${ident(name)}` : ident(name);
  const select = (where, opts = {}) => {
    const params = [];
    const cols = opts.select?.length ? opts.select.map(ident).join(", ") : "*";
    let text = `SELECT ${cols} FROM ${t}` + compileWhere(where, params, d) + compileOrder(opts.orderBy, d);
    if (opts.limit !== undefined) text += ` LIMIT ${whole(opts.limit, "limit")}`;
    if (opts.offset !== undefined) text += (opts.limit === undefined ? ` LIMIT ${d.all}` : "") + ` OFFSET ${whole(opts.offset, "offset")}`;
    return query(conn, text, params);
  };
  const setClause = (set, params) => {
    const keys = Object.keys(set ?? {});
    if (!keys.length) throw new Error("Nothing to set");
    return keys.map((k) => (params.push(set[k]), `${ident(k)} = ${mark(params)}`)).join(", ");
  };
  return {
    find: select,
    findOne: async (where, opts = {}) => (await select(where, { ...opts, limit: 1 }))[0] ?? null,
    count: async (where) => {
      const params = [];
      const rows = await query(conn, `SELECT COUNT(*) AS n FROM ${t}` + compileWhere(where, params, d), params);
      return Number(rows[0].n);
    },
    insert: (row, opts = {}) => {
      const list = Array.isArray(row) ? row : [row];
      if (!list.length) throw new Error("Nothing to insert");
      const keys = Object.keys(list[0]);
      if (!keys.length) throw new Error("A row needs at least one column");
      if (list.some((r) => Object.keys(r).length !== keys.length || keys.some((k) => !(k in r)))) throw new Error("Every row must have the same columns");
      const params = [];
      const one = (r) => `(${keys.map((k) => (params.push(r[k]), mark(params))).join(", ")})`;
      const text = `INSERT INTO ${t} (${keys.map(ident).join(", ")}) VALUES ${list.map(one).join(", ")}`;
      if (!pg) return query(conn, text, params);
      // RETURNING is opt-in: it needs SELECT privilege and visibility of the new row, which a valid insert may lack, and an
      // answer cut after the row was committed would invite a duplicate retry.
      const ret = opts?.returning;
      if (ret === undefined || ret === false) return query(conn, text, params);
      if (ret !== true && !(Array.isArray(ret) && ret.length && ret.every((c) => typeof c === "string"))) throw new Error("returning must be true or a list of column names");
      // Every argument error above throws before this point, synchronously like the MySQL path, not as a rejected promise.
      // The relay sets affected, and insertId from the first returned column of a single row: it knows the column
      // order, which the row object here does not keep.
      return query(conn, text + " RETURNING " + (ret === true ? "*" : ret.map(ident).join(", ")), params);
    },
    update: (where, set) => {
      if (where === undefined) throw new Error("update needs a where. Pass {} to change every row.");
      const params = [];
      const sets = setClause(set, params);
      return query(conn, `UPDATE ${t} SET ${sets}` + compileWhere(where, params, d), params);
    },
    delete: (where) => {
      if (where === undefined) throw new Error("delete needs a where. Pass {} to remove every row.");
      const params = [];
      return query(conn, `DELETE FROM ${t}` + compileWhere(where, params, d), params);
    },
  };
}

// ponytail: one guard covers every Mongo client in the run because the driver's prototypes are shared. Make it
// per client when someone needs to write to one Mongo while reading another.
function guardMongo() {
  const { MongoClient, Collection, Db, Admin, AggregationCursor } = mongodb;
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
function mongoHelpers() {
  const BsonObjectId = mongodb.ObjectId;
  function ObjectId(...a) { return new BsonObjectId(...a); }
  function ISODate(s) { return s ? new Date(s) : new Date(); }
  // The original's randomId alphabet: digits and letters of both cases.
  const ALPHANUM = "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
  const randomId = (n = 17) => [...crypto.getRandomValues(new Uint8Array(n))].map((b) => ALPHANUM[b % 62]).join("");
  const UUIDv4 = () => crypto.randomUUID();
  return { ObjectId, ISODate, randomId, UUIDv4 };
}

// Commands that are the driver's own housekeeping, not something the script asked for.
const QUIET = new Set(["hello", "isMaster", "ismaster", "ping", "endSessions", "killCursors", "getMore", "saslStart", "saslContinue", "buildInfo", "buildinfo"]);
const TEXT_MAX = 2000;

// Only these name a collection in their command document; for any other command a string value is an argument (dropUser: "u1").
const COLLECTION_COMMANDS = new Set(["find", "aggregate", "count", "distinct", "insert", "update", "delete", "findAndModify", "createIndexes", "dropIndexes", "drop", "listIndexes", "collMod"]);
const WRITE_COMMANDS = new Set(["insert", "update", "delete", "findAndModify"]);

// users.find({ legacyId: ... }) for a command document, the way the script would have written it.
function commandText(name, cmd) {
  const coll = COLLECTION_COMMANDS.has(name) && typeof cmd[name] === "string" ? cmd[name] : "";
  const ops = (list, key) => (list && list.length === 1 ? list[0][key] : list?.map((x) => x[key]));
  const arg = {
    find: cmd.filter, count: cmd.query, distinct: cmd.query, aggregate: cmd.pipeline,
    update: ops(cmd.updates, "q"), delete: ops(cmd.deletes, "q"),
  }[name];
  const body = name === "insert" ? `${cmd.documents?.length ?? 0} documents` : arg === undefined ? "" : mongodb.BSON.EJSON.stringify(arg, { relaxed: true });
  const text = `${coll || "db"}.${name}(${body})`;
  // Cut on code points of the text, then repair a pair the cut split: serde_json refuses a lone surrogate.
  return text.length > TEXT_MAX ? text.slice(0, TEXT_MAX).toWellFormed() : text;
}

// Each client reports its commands to the Queries panel under the connection's name.
function watchCommands(client, conn) {
  const started = new Map();
  client.on("commandStarted", (ev) => {
    if (!QUIET.has(ev.commandName)) started.set(ev.requestId, ev.command);
  });
  const done = (ev, failed) => {
    const cmd = started.get(ev.requestId);
    if (!cmd) return;
    started.delete(ev.requestId);
    const q = { type: "query", conn, text: commandText(ev.commandName, cmd), ms: Math.round(ev.duration) };
    const cursor = ev.reply?.cursor;
    // Later batches come through getMore, so a first batch alone would show a wrong row count.
    if (Array.isArray(cursor?.firstBatch)) {
      if (String(cursor.id) === "0") q.rows = cursor.firstBatch.length;
    } else if (WRITE_COMMANDS.has(ev.commandName)) {
      const n = ev.commandName === "findAndModify" ? ev.reply?.lastErrorObject?.n : ev.reply?.n;
      if (typeof n === "number") q.affected = n;
    } else if (typeof ev.reply?.n === "number") q.rows = ev.reply.n;
    // The driver replaces the failure of a sensitive command with a bare object.
    if (failed) q.error = String(ev.failure?.message ?? ev.failure?.codeName ?? "failed");
    out(q);
  };
  client.on("commandSucceeded", (ev) => done(ev));
  client.on("commandFailed", (ev) => done(ev, true));
}

const isName = (n) => /^[A-Za-z_$][\w$]*$/.test(n) && !RESERVED.has(n);

const input = await inputLine;
const mongoEntries = input.mongo || [];
const sqlEntries = input.sql || [];
const clients = [];
// Query time starts at the first checkout, once connected and signed in; CMAP events need no command monitoring, which slows reads.
let firstOut;
try {
  // Names pick the connection a query goes to, so a repeated one would send queries to the wrong place.
  const seen = new Set();
  for (const n of [...mongoEntries, ...sqlEntries].map((e) => e.name)) {
    if (seen.has(n)) throw new Error(`The connection name "${n}" is used more than once in this script. Give each connection its own name.`);
    seen.add(n);
  }
  // ponytail: monitoring only with attachments, because it makes the driver decode every cursor reply twice (about 30% slower).
  // Add a per-run opt-in if someone wants Mongo queries listed for a single connection.
  const monitor = mongoEntries.length + sqlEntries.length > 1;
  const ctx = { require, console: console2, log: console2.log };
  if (mongoEntries.length) {
    mongodb = await import("mongodb");
    // Shared by every client, so apply once, and before any client exists.
    if (mongoEntries.some((m) => m.readOnly)) guardMongo();
    Object.assign(ctx, mongoHelpers());
  }
  const open = (m) => {
    // SSH-only servers are reached through Quarry's tunnel (a local SOCKS5 port) or the connection's
    // own proxy; the driver needs the "socks" package for that, installed as a built-in.
    // The socket timeout bounds a reply that never comes: a tunnel, VPN or port forward that stalls after the handshake
    // otherwise leaves the run waiting forever, since server selection only covers connecting.
    const options = { appName: "quarry-shell", serverSelectionTimeoutMS: input.timeoutMs, socketTimeoutMS: input.timeoutMs, monitorCommands: monitor };
    if (m.proxy) {
      options.proxyHost = m.proxy.host;
      options.proxyPort = m.proxy.port;
      if (m.proxy.user) {
        options.proxyUsername = m.proxy.user;
        options.proxyPassword = m.proxy.password || "";
      }
    }
    // The driver connects on the first database call, so plain scripts run without a server.
    const client = new mongodb.MongoClient(m.uri, options);
    clients.push(client);
    client.once("connectionCheckedOut", () => (firstOut ??= performance.now()));
    if (monitor) watchCommands(client, m.name);
    return client.db(m.db);
  };
  // Attached handles are added after everything else the run defines, so a clash is found whatever the order of the input.
  const attachments = [];
  const attach = (name, value) => attachments.push([name, value]);
  // The main entry's things are plain globals; every other entry is one global under its name.
  for (const m of mongoEntries) {
    const db = open(m);
    if (m.name === input.main) {
      ctx.db = db;
      for (const name of m.collections || []) {
        if (isName(name) && !(name in ctx)) ctx[name] = db.collection(name);
      }
    } else {
      // Collections hang off the Db object so travels.users.find() works; a Db method keeps its name.
      for (const name of m.collections || []) {
        if (isName(name) && !(name in db)) db[name] = db.collection(name);
      }
      attach(m.name, db);
    }
  }
  for (const s of sqlEntries) dialects.set(s.name, s.dialect);
  for (const s of sqlEntries) {
    const handle = makeSql(s.name);
    if (s.name === input.main) {
      ctx.sql = handle;
      ctx.table = handle.table;
      for (const name of s.tables || []) {
        if (isName(name) && !(name in ctx)) ctx[name] = makeTable(s.name, name);
      }
    } else {
      for (const name of s.tables || []) {
        if (isName(name) && !(name in handle)) handle[name] = makeTable(s.name, name);
      }
      attach(s.name, handle);
    }
  }
  // Built in, like the original shell: _ (lodash), dayjs and UUIDv1.
  try {
    ctx._ = require("lodash");
    ctx.dayjs = require("dayjs");
    if (mongodb) ctx.UUIDv1 = require("uuid").v1;
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
  for (const [name, value] of attachments) {
    if (!isName(name)) throw new Error(`"${name}" cannot be a connection name in the shell, it is a reserved word or not a valid name.`);
    if (name in ctx) throw new Error(`The connection "${name}" cannot be attached because the shell already defines "${name}" (a built-in, a helper, a package, or a collection or table of the main connection). Rename the connection.`);
    ctx[name] = value;
  }
  const fn = new Function(...Object.keys(ctx), `return (${input.code.trim()})`)(...Object.values(ctx));
  if (typeof fn !== "function") throw new Error("The command is not a function. Write it as: async function run() { ... }");
  let result = await fn();
  if (result === null) throw new Error("No results");
  if (result === undefined) throw new Error('No results. Did you forget to "return" something in the function?');
  let cursor = null;
  if (result && typeof result.toArray === "function") {
    // limit() would replace a find cursor's own limit. findOptions is driver state, not public API.
    // On an aggregation it appends a $limit stage, which may not follow $out or $merge.
    const last = Array.isArray(result.pipeline) ? result.pipeline.at(-1) : undefined;
    const writes = last && typeof last === "object" && ("$out" in last || "$merge" in last);
    // listCollections and listIndexes cursors have no limit and are read whole.
    if (typeof result.limit === "function" && !result.findOptions?.limit && !writes) result.limit(100);
    if (input.maxBytes) cursor = result;
    else result = await result.toArray();
  }
  const queryMs = () => (firstOut === undefined ? undefined : Math.round(performance.now() - firstOut));
  const docs = input.maxBytes && (cursor || (Array.isArray(result) && result.length && result.every(isDoc)) || isDoc(result));
  // A list the script already holds was read; sending it is not query time. A cursor is read while it is sent.
  const readMs = cursor ? undefined : queryMs();
  const sent = docs ? await sendDocs(cursor ?? result) : null;
  if (sent) await write({ type: "result", ok: true, ...sent, queryMs: readMs ?? queryMs() });
  // A cursor that found nothing is an empty list, shown as one.
  else await sendText(cursor ? [] : result, queryMs());
} catch (e) {
  await write({ type: "result", ok: false, error: String(e && e.message ? e.message : e) });
} finally {
  await Promise.all(clients.map((c) => c.close().catch(() => {})));
}
process.exit(0);
