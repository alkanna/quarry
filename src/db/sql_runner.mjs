// Runs one SQL Shell script for Quarry under Bun. The script gets no driver and no password: every
// query goes to Quarry, which runs it on one connection with the connection's read-only rules.
// stdin (JSON lines): the first line is { db, code, tables: [names], packages, timeoutMs }; after it
//   come the answers { type: "sqlres", id, ok, rows | affected, insertId | error }.
// stdout (JSON lines): { type: "log", level, text }, { type: "sql", id, text, params } for each query,
//   then { type: "result", ok: true, ejson } or { type: "result", ok: false, error }.
import { createRequire } from "module";

const require = createRequire(import.meta.url);

const write = (o) => new Promise((r) => process.stdout.write(JSON.stringify(o) + "\n", r));
const out = (o) => void write(o);
// Rows hold bigints and bytes that JSON.stringify would refuse or spell out as an object of numbers.
const replacer = (_, v) => {
  if (typeof v === "bigint") return Number.isSafeInteger(Number(v)) ? Number(v) : v.toString();
  if (v instanceof Uint8Array) return { $binary: { base64: Buffer.from(v).toString("base64"), subType: "00" } };
  return v;
};
const fmt = (m) => (typeof m === "string" ? m : JSON.stringify(m, replacer, 2) ?? String(m));
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
        if (v.$binary) return Buffer.from(v.$binary.base64, "base64");
        if (typeof v.$numberLong === "string") return BigInt(v.$numberLong);
      }
      return v;
    });
    const p = pending.get(m.id);
    if (!p) return;
    pending.delete(m.id);
    if (!m.ok) return p.reject(new Error(m.error));
    const rows = m.rows ?? [];
    rows.affected = m.affected ?? 0;
    rows.insertId = m.insertId ?? 0;
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

// What a value becomes on the wire. Quarry turns each one into a literal; nothing here is spliced into text.
function wire(v) {
  if (v === undefined || v === null) return null;
  if (typeof v === "boolean") return v ? 1 : 0;
  if (typeof v === "bigint") return v >= -9007199254740991n && v <= 9007199254740991n ? Number(v) : { $int: v.toString() };
  if (typeof v === "number") {
    if (!Number.isFinite(v)) throw new Error("A query value is not a finite number");
    // Past 2^53 the number is already rounded, so the digits the script meant are gone. Use a BigInt.
    if (Number.isInteger(v) && !Number.isSafeInteger(v)) throw new Error("A query value is an integer beyond 2^53 that a number cannot hold. Pass it as a BigInt, like 123n");
    return v;
  }
  if (v instanceof Date) {
    if (Number.isNaN(v.getTime())) throw new Error("A query value is an invalid Date");
    return v.toISOString().replace("T", " ").replace("Z", "");
  }
  if (v instanceof Uint8Array) return { $hex: Buffer.from(v).toString("hex") };
  if (typeof v === "object") return JSON.stringify(v, replacer);
  return v;
}

function query(text, params) {
  const id = ++nextId;
  let wired;
  try {
    wired = params.map(wire);
  } catch (e) {
    return Promise.reject(e);
  }
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    out({ type: "sql", id, text, params: wired });
  });
}

// sql`... ${v} ...` becomes text with ? placeholders. An array expands to one ? per item, for IN (...).
function sql(strings, ...values) {
  let text = strings[0];
  const params = [];
  values.forEach((v, i) => {
    if (Array.isArray(v)) {
      text += v.length ? v.map(() => "?").join(", ") : "NULL";
      params.push(...v);
    } else {
      text += "?";
      params.push(v);
    }
    text += strings[i + 1];
  });
  return query(text, params);
}
sql.unsafe = (text, params = []) => query(String(text), params);

const ident = (n) => "`" + String(n).replaceAll("`", "``") + "`";

const isPlain = (v) => v !== null && typeof v === "object" && (Object.getPrototypeOf(v) === Object.prototype || Object.getPrototypeOf(v) === null);

// where: { col: value } is =, null is IS NULL, an array is IN, and { gt, gte, lt, lte, ne, like, in } compare.
// Anything unclear throws: a condition that silently vanishes would turn an update or delete into every row.
function compileWhere(where, params) {
  if (where === undefined) return "";
  if (!isPlain(where)) throw new Error("where must be an object like { id: 1 }. Pass {} for every row.");
  const parts = [];
  const eq = (c, v) => {
    if (v === undefined) throw new Error(`The value for "${c}" is undefined. Use null to match NULL.`);
    if (v === null) return `${ident(c)} IS NULL`;
    if (Array.isArray(v)) {
      if (v.includes(undefined)) throw new Error(`A list for "${c}" holds undefined`);
      params.push(...v);
      return v.length ? `${ident(c)} IN (${v.map(() => "?").join(", ")})` : "FALSE";
    }
    if (typeof v === "object" && !(v instanceof Date || v instanceof Uint8Array)) throw new Error(`The value for "${c}" is not a plain value, a list or a condition object`);
    params.push(v);
    return `${ident(c)} = ?`;
  };
  const ops = { gt: ">", gte: ">=", lt: "<", lte: "<=", like: "LIKE" };
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
        parts.push(`${ident(c)} ${ops[op]} ?`);
      } else if (op === "ne") {
        parts.push(x === null ? `${ident(c)} IS NOT NULL` : `${ident(c)} <> ?`);
        if (x !== null) params.push(x);
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
function compileOrder(o) {
  if (o === undefined) return "";
  const items = [];
  for (const x of Array.isArray(o) ? o : [o]) {
    if (typeof x === "string") items.push(x.startsWith("-") ? `${ident(x.slice(1))} DESC` : ident(x));
    else for (const [c, d] of Object.entries(x)) items.push(`${ident(c)} ${String(d).toLowerCase() === "desc" ? "DESC" : "ASC"}`);
  }
  return items.length ? " ORDER BY " + items.join(", ") : "";
}

function table(name) {
  const t = ident(name);
  const select = (where, opts = {}) => {
    const params = [];
    const cols = opts.select?.length ? opts.select.map(ident).join(", ") : "*";
    let text = `SELECT ${cols} FROM ${t}` + compileWhere(where, params) + compileOrder(opts.orderBy);
    if (opts.limit !== undefined) text += ` LIMIT ${whole(opts.limit, "limit")}`;
    if (opts.offset !== undefined) text += (opts.limit === undefined ? " LIMIT 18446744073709551615" : "") + ` OFFSET ${whole(opts.offset, "offset")}`;
    return query(text, params);
  };
  const setClause = (set, params) => {
    const keys = Object.keys(set ?? {});
    if (!keys.length) throw new Error("Nothing to set");
    params.push(...keys.map((k) => set[k]));
    return keys.map((k) => `${ident(k)} = ?`).join(", ");
  };
  return {
    find: select,
    findOne: async (where, opts = {}) => (await select(where, { ...opts, limit: 1 }))[0] ?? null,
    count: async (where) => {
      const params = [];
      const rows = await query(`SELECT COUNT(*) AS n FROM ${t}` + compileWhere(where, params), params);
      return Number(rows[0].n);
    },
    insert: (row) => {
      const list = Array.isArray(row) ? row : [row];
      if (!list.length) throw new Error("Nothing to insert");
      const keys = Object.keys(list[0]);
      if (!keys.length) throw new Error("A row needs at least one column");
      if (list.some((r) => Object.keys(r).length !== keys.length || keys.some((k) => !(k in r)))) throw new Error("Every row must have the same columns");
      const one = `(${keys.map(() => "?").join(", ")})`;
      return query(`INSERT INTO ${t} (${keys.map(ident).join(", ")}) VALUES ${list.map(() => one).join(", ")}`, list.flatMap((r) => keys.map((k) => r[k])));
    },
    update: (where, set) => {
      if (where === undefined) throw new Error("update needs a where. Pass {} to change every row.");
      const params = [];
      const sets = setClause(set, params);
      return query(`UPDATE ${t} SET ${sets}` + compileWhere(where, params), params);
    },
    delete: (where) => {
      if (where === undefined) throw new Error("delete needs a where. Pass {} to remove every row.");
      const params = [];
      return query(`DELETE FROM ${t}` + compileWhere(where, params), params);
    },
  };
}

function packageIdent(name) {
  const base = name.replace(/^@[^/]+\//, "");
  let id = base.split(/[^A-Za-z0-9]+/).filter(Boolean).map((p, i) => (i ? p[0].toUpperCase() + p.slice(1) : p)).join("");
  if (/^[0-9]/.test(id)) id = "_" + id;
  return id || "pkg";
}

// Words that cannot be parameter names. Must match JS_RESERVED in shell.rs.
const RESERVED = new Set(["break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do",
  "else", "enum", "export", "extends", "false", "finally", "for", "function", "if", "import", "in", "instanceof", "new",
  "null", "return", "super", "switch", "this", "throw", "true", "try", "typeof", "var", "void", "while", "with",
  "implements", "interface", "let", "package", "private", "protected", "public", "static", "yield", "await",
  "arguments", "eval"]);

const input = await inputLine;
let code = 0;
try {
  const ctx = { sql, table, require, console: console2, log: console2.log };
  for (const name of input.tables) {
    if (/^[A-Za-z_$][\w$]*$/.test(name) && !RESERVED.has(name) && !(name in ctx)) ctx[name] = table(name);
  }
  try {
    ctx._ = require("lodash");
    ctx.dayjs = require("dayjs");
  } catch (e) {
    console2.warn(`Could not load the built-in helpers: ${e && e.message ? e.message : e}`);
  }
  for (const pkg of input.packages || []) {
    const id = packageIdent(pkg);
    try {
      let mod;
      try {
        mod = require(pkg);
      } catch {
        mod = await import(pkg);
      }
      const value = mod && typeof mod === "object" && mod.default !== undefined ? mod.default : mod;
      if (!(id in ctx)) ctx[id] = value;
      if (pkg === "lodash" && !("_" in ctx)) ctx._ = value;
    } catch (e) {
      console2.warn(`Could not load package ${pkg}: ${e && e.message ? e.message : e}`);
    }
  }
  const fn = new Function(...Object.keys(ctx), `return (${input.code.trim()})`)(...Object.values(ctx));
  if (typeof fn !== "function") throw new Error("The command is not a function. Write it as: async function run() { ... }");
  const result = await fn();
  if (result === null) throw new Error("No results");
  if (result === undefined) throw new Error('No results. Did you forget to "return" something in the function?');
  await write({ type: "result", ok: true, ejson: JSON.stringify({ v: result }, replacer) });
} catch (e) {
  await write({ type: "result", ok: false, error: String(e && e.message ? e.message : e) });
  code = 0;
}
process.exit(code);
