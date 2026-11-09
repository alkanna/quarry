//! "Copy as": rows turned into text in a chosen format. Pure functions over documents and the visible
//! column list, so Mongo rows and SQL rows share them and they can be tested without a window.

use super::*;

const DIALECT: sql::Dialect = sql::Dialect::MySql;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    SqlInsert,
    SqlUpdate,
    Csv,
    Tsv,
    Json,
    /// One compact object per line.
    JsonLines,
    Markdown,
    Html,
    Values,
}

/// One visible column. `numeric` right-aligns it in Markdown and leaves numbers unquoted in SQL and JSON.
#[derive(Clone, Debug)]
pub struct CopyCol {
    /// The key in the row document, the name shown.
    pub name: String,
    /// The server's own column name, which SQL statements must use. Equals `name` unless a result renamed the column.
    pub src: String,
    pub numeric: bool,
}

/// What to format: the rows (full documents, a SQL row has its key in `_id`), the columns in the order shown,
/// and for SQL the table and its key columns. An empty `pk` means there is no UPDATE to build.
pub struct Sheet<'a> {
    pub rows: &'a [Document],
    pub cols: &'a [CopyCol],
    pub table: &'a str,
    pub pk: &'a [String],
    /// Whether the server reads backslash escapes in strings; false under NO_BACKSLASH_ESCAPES.
    pub bs: bool,
}

/// Text of one cell, None for NULL or a missing field.
fn text(b: Option<&Bson>) -> Option<String> {
    Some(match b? {
        Bson::Null | Bson::Undefined => return None,
        Bson::String(s) => s.clone(),
        Bson::ObjectId(o) => o.to_hex(),
        Bson::DateTime(t) => t.try_to_rfc3339_string().unwrap_or_default(),
        Bson::Binary(b) => format!("0x{}", sql::value::hex_digits(&b.bytes, true)),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(f) => f.to_string(),
        Bson::Boolean(b) => b.to_string(),
        other => compact(other),
    })
}

fn value<'a>(d: &'a Document, c: &CopyCol) -> Option<&'a Bson> {
    // A SQL column can hold a dot in its name, so the exact key wins over a path.
    d.get(&c.name).or_else(|| get_path(d, &c.name))
}

fn plain_number(s: &str) -> bool {
    let t = s.strip_prefix(['-', '+']).unwrap_or(s);
    !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit() || b == b'.' || b == b'e' || b == b'E' || b == b'-' || b == b'+') && t.bytes().next().is_some_and(|b| b.is_ascii_digit() || b == b'.') && s.parse::<f64>().is_ok_and(f64::is_finite)
}

/// `bs` is false when the server has NO_BACKSLASH_ESCAPES, where doubling a backslash would change the text.
pub fn sql_literal(b: Option<&Bson>, numeric: bool, bs: bool) -> String {
    match b {
        None | Some(Bson::Null) | Some(Bson::Undefined) => "NULL".into(),
        Some(Bson::Int32(_) | Bson::Int64(_)) => text(b).unwrap_or_default(),
        Some(Bson::Double(f)) if f.is_finite() => f.to_string(),
        Some(Bson::Boolean(v)) => if *v { "TRUE" } else { "FALSE" }.into(),
        // A bare 0x is not valid SQL, and '' would turn a binary value into text.
        Some(Bson::Binary(x)) if x.bytes.is_empty() => "X''".into(),
        Some(Bson::Binary(x)) => format!("0x{}", sql::value::hex_digits(&x.bytes, true)),
        Some(Bson::String(s)) if numeric && plain_number(s) => s.clone(),
        _ => {
            sql::value::lit(&text(b).unwrap_or_default(), bs)
        }
    }
}

fn json_literal(b: Option<&Bson>, numeric: bool) -> String {
    match b {
        None | Some(Bson::Null) | Some(Bson::Undefined) => "null".into(),
        Some(Bson::Int32(_) | Bson::Int64(_)) => text(b).unwrap_or_default(),
        Some(Bson::Double(f)) if f.is_finite() => f.to_string(),
        Some(Bson::Boolean(v)) => v.to_string(),
        Some(Bson::String(s)) if numeric && plain_number(s) => s.clone(),
        Some(Bson::Document(_) | Bson::Array(_)) => b.map(compact).unwrap_or_default(),
        _ => serde_json::to_string(&text(b).unwrap_or_default()).unwrap_or_default(),
    }
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace("\r\n", "<br>").replace(['\n', '\r'], "<br>")
}

pub fn html_cell(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Rows as a plain fixed-width table for the Query view's Text mode: a header, a rule and one line per row.
pub fn text_table(cols: &[String], rows: &[Document]) -> String {
    let cells: Vec<Vec<String>> = rows.iter().map(|d| cols.iter().map(|c| text(d.get(c)).map_or_else(|| "NULL".to_string(), |s| s.replace(['\n', '\r'], " "))).collect()).collect();
    let widths: Vec<usize> = cols.iter().enumerate().map(|(i, c)| cells.iter().map(|r| r[i].chars().count()).chain([c.chars().count()]).max().unwrap_or(0)).collect();
    let line = |r: &[String]| r.iter().zip(&widths).map(|(c, w)| format!("{c:<width$}", width = *w)).collect::<Vec<_>>().join("  ").trim_end().to_string();
    let rule = widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("  ");
    let mut out = vec![line(cols), rule];
    out.extend(cells.iter().map(|r| line(r)));
    out.join("\n")
}

/// The text for `f`. `header` is the "Include column names" switch: CSV, TSV and HTML leave out their
/// header row, Markdown keeps a blank one because a table needs it. SQL INSERT always names its columns,
/// since without them the values land in the table's order, not the order shown.
pub fn format(f: Fmt, s: &Sheet, header: bool) -> String {
    let cell = |d: &Document, c: &CopyCol| text(value(d, c)).unwrap_or_default();
    let names: Vec<&str> = s.cols.iter().map(|c| c.name.as_str()).collect();
    match f {
        Fmt::SqlInsert => {
            let mut out = format!("INSERT INTO {}", DIALECT.quote_ident(s.table));
            out.push_str(&format!(" ({})", s.cols.iter().map(|c| DIALECT.quote_ident(&c.src)).collect::<Vec<_>>().join(", ")));
            out.push_str(" VALUES\n");
            let rows: Vec<String> = s.rows.iter().map(|d| format!("({})", s.cols.iter().map(|c| sql_literal(value(d, c), c.numeric, s.bs)).collect::<Vec<_>>().join(", "))).collect();
            out + &rows.join(",\n") + ";"
        }
        Fmt::SqlUpdate => {
            let sets: Vec<&CopyCol> = s.cols.iter().filter(|c| !s.pk.contains(&c.src)).collect();
            let mut out = Vec::new();
            for d in s.rows {
                let Ok(key) = d.get_document("_id") else { continue };
                if sets.is_empty() || s.pk.is_empty() || !s.pk.iter().all(|p| key.contains_key(p)) {
                    continue;
                }
                let set = sets.iter().map(|c| format!("{} = {}", DIALECT.quote_ident(&c.src), sql_literal(value(d, c), c.numeric, s.bs))).collect::<Vec<_>>().join(", ");
                let wh = s.pk.iter().map(|p| format!("{} = {}", DIALECT.quote_ident(p), sql_literal(key.get(p), false, s.bs))).collect::<Vec<_>>().join(" AND ");
                out.push(format!("UPDATE {} SET {set} WHERE {wh};", DIALECT.quote_ident(s.table)));
            }
            out.join("\n")
        }
        Fmt::Csv | Fmt::Tsv => {
            let (sep, esc): (&str, fn(&str) -> String) = if f == Fmt::Csv { (",", csv_cell) } else { ("\t", |x: &str| x.replace(['\t', '\n', '\r'], " ")) };
            let mut lines = Vec::new();
            if header {
                lines.push(names.iter().map(|n| esc(n)).collect::<Vec<_>>().join(sep));
            }
            lines.extend(s.rows.iter().map(|d| s.cols.iter().map(|c| esc(&cell(d, c))).collect::<Vec<_>>().join(sep)));
            lines.join("\n")
        }
        Fmt::Json => {
            if s.rows.is_empty() {
                return "[]".into();
            }
            let rows: Vec<String> = s
                .rows
                .iter()
                .map(|d| {
                    let fields: Vec<String> = s.cols.iter().map(|c| format!("    {}: {}", serde_json::to_string(&c.name).unwrap_or_default(), json_literal(value(d, c), c.numeric))).collect();
                    format!("  {{\n{}\n  }}", fields.join(",\n"))
                })
                .collect();
            format!("[\n{}\n]", rows.join(",\n"))
        }
        Fmt::JsonLines => s.rows.iter().map(|d| json_line(d, s.cols)).collect::<Vec<_>>().join("\n"),
        Fmt::Markdown => {
            let head: Vec<String> = s.cols.iter().map(|c| if header { md_cell(&c.name) } else { String::new() }).collect();
            let body: Vec<Vec<String>> = s.rows.iter().map(|d| s.cols.iter().map(|c| md_cell(&cell(d, c))).collect()).collect();
            // At least three dashes, or the line is not a delimiter row.
            let widths: Vec<usize> = (0..s.cols.len()).map(|i| body.iter().map(|r| r[i].chars().count()).chain([head[i].chars().count(), 3]).max().unwrap_or(3)).collect();
            let pad = |t: &str, i: usize| {
                let gap = " ".repeat(widths[i] - t.chars().count());
                if s.cols[i].numeric { format!("{gap}{t}") } else { format!("{t}{gap}") }
            };
            let line = |cells: Vec<String>| format!("| {} |", cells.join(" | "));
            let mut out = vec![line(head.iter().enumerate().map(|(i, t)| format!("{t}{}", " ".repeat(widths[i] - t.chars().count()))).collect())];
            out.push(line((0..s.cols.len()).map(|i| if s.cols[i].numeric { format!("{}:", "-".repeat(widths[i] - 1)) } else { "-".repeat(widths[i]) }).collect()));
            out.extend(body.iter().map(|r| line(r.iter().enumerate().map(|(i, t)| pad(t, i)).collect())));
            out.join("\n")
        }
        Fmt::Html => {
            let mut out = String::from("<table>\n");
            if header {
                out.push_str(&format!("  <thead><tr>{}</tr></thead>\n", names.iter().map(|n| format!("<th>{}</th>", html_cell(n))).collect::<String>()));
            }
            out.push_str("  <tbody>\n");
            for d in s.rows {
                out.push_str(&format!("    <tr>{}</tr>\n", s.cols.iter().map(|c| format!("<td>{}</td>", html_cell(&cell(d, c)))).collect::<String>()));
            }
            out + "  </tbody>\n</table>"
        }
        Fmt::Values => s.rows.iter().flat_map(|d| s.cols.iter().map(move |c| cell(d, c))).collect::<Vec<_>>().join("\n"),
    }
}

fn json_line(d: &Document, cols: &[CopyCol]) -> String {
    format!("{{{}}}", cols.iter().map(|c| format!("{}:{}", serde_json::to_string(&c.name).unwrap_or_default(), json_literal(value(d, c), c.numeric))).collect::<Vec<_>>().join(","))
}

/// `format` in pieces for results too big to hold: `begin`, `rows` for each batch, then `end`. The
/// pieces joined are the bytes `format` gives for the same rows, except that a Markdown table is not
/// padded to the widest cell, which is unknown until the last row.
pub struct Writer {
    f: Fmt,
    header: bool,
    cols: Vec<CopyCol>,
    table: String,
    pk: Vec<String>,
    bs: bool,
    /// Something was written, so the next line or row needs its separator first.
    any: bool,
}

impl Writer {
    pub fn new(f: Fmt, header: bool, cols: Vec<CopyCol>, table: &str, pk: &[String], bs: bool) -> Writer {
        Writer { f, header, cols, table: table.to_string(), pk: pk.to_vec(), bs, any: false }
    }

    /// Lines of text are joined by newlines, so the first has none in front.
    fn line(&mut self, s: String) -> String {
        let out = if self.any { format!("\n{s}") } else { s };
        self.any = true;
        out
    }

    pub fn begin(&mut self) -> String {
        let names: Vec<&str> = self.cols.iter().map(|c| c.name.as_str()).collect();
        match self.f {
            Fmt::SqlInsert => format!("INSERT INTO {} ({}) VALUES\n", DIALECT.quote_ident(&self.table), self.cols.iter().map(|c| DIALECT.quote_ident(&c.src)).collect::<Vec<_>>().join(", ")),
            Fmt::Csv | Fmt::Tsv if self.header => {
                let (sep, esc): (&str, fn(&str) -> String) = if self.f == Fmt::Csv { (",", csv_cell) } else { ("\t", |x: &str| x.replace(['\t', '\n', '\r'], " ")) };
                let l = names.iter().map(|n| esc(n)).collect::<Vec<_>>().join(sep);
                self.line(l)
            }
            Fmt::Json => "[".into(),
            Fmt::Markdown => {
                let head = self.cols.iter().map(|c| if self.header { md_cell(&c.name) } else { String::new() }).map(|t| format!("{t:<3}")).collect::<Vec<_>>().join(" | ");
                let rule = self.cols.iter().map(|c| if c.numeric { "--:" } else { "---" }).collect::<Vec<_>>().join(" | ");
                let a = self.line(format!("| {head} |"));
                a + &self.line(format!("| {rule} |"))
            }
            Fmt::Html => {
                let mut out = String::from("<table>\n");
                if self.header {
                    out.push_str(&format!("  <thead><tr>{}</tr></thead>\n", names.iter().map(|n| format!("<th>{}</th>", html_cell(n))).collect::<String>()));
                }
                out + "  <tbody>\n"
            }
            _ => String::new(),
        }
    }

    pub fn rows(&mut self, rows: &[Document]) -> String {
        let mut out = String::new();
        let cell = |d: &Document, c: &CopyCol| text(value(d, c)).unwrap_or_default();
        for d in rows {
            match self.f {
                Fmt::SqlInsert => {
                    let r = format!("({})", self.cols.iter().map(|c| sql_literal(value(d, c), c.numeric, self.bs)).collect::<Vec<_>>().join(", "));
                    out.push_str(&if self.any { format!(",\n{r}") } else { r });
                    self.any = true;
                }
                Fmt::SqlUpdate => {
                    let sets: Vec<&CopyCol> = self.cols.iter().filter(|c| !self.pk.contains(&c.src)).collect();
                    let Ok(key) = d.get_document("_id") else { continue };
                    if sets.is_empty() || self.pk.is_empty() || !self.pk.iter().all(|p| key.contains_key(p)) {
                        continue;
                    }
                    let set = sets.iter().map(|c| format!("{} = {}", DIALECT.quote_ident(&c.src), sql_literal(value(d, c), c.numeric, self.bs))).collect::<Vec<_>>().join(", ");
                    let wh = self.pk.iter().map(|p| format!("{} = {}", DIALECT.quote_ident(p), sql_literal(key.get(p), false, self.bs))).collect::<Vec<_>>().join(" AND ");
                    let l = self.line(format!("UPDATE {} SET {set} WHERE {wh};", DIALECT.quote_ident(&self.table)));
                    out.push_str(&l);
                }
                Fmt::Csv | Fmt::Tsv => {
                    let (sep, esc): (&str, fn(&str) -> String) = if self.f == Fmt::Csv { (",", csv_cell) } else { ("\t", |x: &str| x.replace(['\t', '\n', '\r'], " ")) };
                    let l = self.line(self.cols.iter().map(|c| esc(&cell(d, c))).collect::<Vec<_>>().join(sep));
                    out.push_str(&l);
                }
                Fmt::Json => {
                    let fields: Vec<String> = self.cols.iter().map(|c| format!("    {}: {}", serde_json::to_string(&c.name).unwrap_or_default(), json_literal(value(d, c), c.numeric))).collect();
                    out.push_str(&format!("{}  {{\n{}\n  }}", if self.any { ",\n" } else { "\n" }, fields.join(",\n")));
                    self.any = true;
                }
                Fmt::JsonLines => {
                    let l = self.line(json_line(d, &self.cols));
                    out.push_str(&l);
                }
                Fmt::Markdown => {
                    let cells = self.cols.iter().map(|c| md_cell(&cell(d, c))).map(|t| format!("{t:<3}")).collect::<Vec<_>>().join(" | ");
                    let l = self.line(format!("| {cells} |"));
                    out.push_str(&l);
                }
                Fmt::Html => {
                    out.push_str(&format!("    <tr>{}</tr>\n", self.cols.iter().map(|c| format!("<td>{}</td>", html_cell(&cell(d, c)))).collect::<String>()));
                    self.any = true;
                }
                Fmt::Values => {
                    for c in self.cols.clone() {
                        let l = self.line(cell(d, &c));
                        out.push_str(&l);
                    }
                }
            }
        }
        out
    }

    pub fn end(&mut self) -> String {
        match self.f {
            Fmt::SqlInsert => ";".into(),
            Fmt::Json => if self.any { "\n]" } else { "]" }.into(),
            Fmt::Html => "  </tbody>\n</table>".into(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<CopyCol> {
        vec![CopyCol { name: "id".into(), src: "id".into(), numeric: true }, CopyCol { name: "status".into(), src: "status".into(), numeric: false }, CopyCol { name: "total".into(), src: "total".into(), numeric: true }]
    }

    fn rows() -> Vec<Document> {
        vec![doc! { "_id": { "id": 1_i64 }, "id": 1_i64, "status": "it's", "total": "26.75" }, doc! { "_id": { "id": 2_i64 }, "id": 2_i64, "status": "a|b\nc", "total": Bson::Null }]
    }

    #[test]
    fn sql_copy_uses_the_servers_column_name() {
        let c = [CopyCol { name: "_id_2".into(), src: "_id".into(), numeric: true }, CopyCol { name: "a".into(), src: "a".into(), numeric: false }];
        let r = [doc! { "_id": { "_id": 1 }, "_id_2": 1, "a": "x" }];
        let pk = vec!["_id".to_string()];
        let sheet = Sheet { rows: &r, cols: &c, table: "t", pk: &pk, bs: true };
        assert_eq!(format(Fmt::SqlInsert, &sheet, true), "INSERT INTO `t` (`_id`, `a`) VALUES\n(1, 'x');");
        assert_eq!(format(Fmt::SqlUpdate, &sheet, true), "UPDATE `t` SET `a` = 'x' WHERE `_id` = 1;");
    }

    #[test]
    fn text_table_pads_columns() {
        let names = vec!["id".to_string(), "status".to_string()];
        assert_eq!(text_table(&names, &rows()), "id  status\n--  ------\n1   it's\n2   a|b c");
        let nulls = [doc! { "id": 7_i64 }];
        assert_eq!(text_table(&names, &nulls), "id  status\n--  ------\n7   NULL");
    }

    fn out(f: Fmt, header: bool) -> String {
        let (c, r) = (cols(), rows());
        let pk = vec!["id".to_string()];
        format(f, &Sheet { rows: &r, cols: &c, table: "orders", pk: &pk, bs: true }, header)
    }

    #[test]
    fn sql_insert_quotes_text_and_leaves_numbers() {
        assert_eq!(out(Fmt::SqlInsert, true), "INSERT INTO `orders` (`id`, `status`, `total`) VALUES\n(1, 'it''s', 26.75),\n(2, 'a|b\nc', NULL);");
        // The column list stays whatever the header switch says, so hidden or moved columns land right.
        assert!(out(Fmt::SqlInsert, false).starts_with("INSERT INTO `orders` (`id`, `status`, `total`) VALUES\n(1,"));
    }

    #[test]
    fn literals_follow_the_server_escape_mode() {
        let s = Bson::String("a\\b'c".into());
        assert_eq!(sql_literal(Some(&s), false, true), "'a\\\\b''c'");
        assert_eq!(sql_literal(Some(&s), false, false), "'a\\b''c'");
        let empty = Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: vec![] });
        assert_eq!(sql_literal(Some(&empty), false, true), "X''");
    }

    #[test]
    fn sql_update_sets_non_key_columns_by_key() {
        assert_eq!(out(Fmt::SqlUpdate, true), "UPDATE `orders` SET `status` = 'it''s', `total` = 26.75 WHERE `id` = 1;\nUPDATE `orders` SET `status` = 'a|b\nc', `total` = NULL WHERE `id` = 2;");
        let (c, r) = (cols(), rows());
        assert_eq!(format(Fmt::SqlUpdate, &Sheet { rows: &r, cols: &c, table: "t", pk: &[], bs: true }, true), "");
    }

    #[test]
    fn csv_and_tsv() {
        let csv = out(Fmt::Csv, true);
        assert_eq!(csv, "id,status,total\n1,it's,26.75\n2,\"a|b\nc\",");
        assert_eq!(out(Fmt::Csv, false).lines().next(), Some("1,it's,26.75"));
        assert_eq!(out(Fmt::Tsv, true), "id\tstatus\ttotal\n1\tit's\t26.75\n2\ta|b c\t");
        let c = [CopyCol { name: "a,\"b\"".into(), src: "a,\"b\"".into(), numeric: false }];
        let r = [doc! { "a,\"b\"": "x,y" }];
        assert_eq!(format(Fmt::Csv, &Sheet { rows: &r, cols: &c, table: "", pk: &[], bs: true }, true), "\"a,\"\"b\"\"\"\n\"x,y\"");
    }

    #[test]
    fn json_is_an_array_of_objects() {
        assert_eq!(out(Fmt::Json, true), "[\n  {\n    \"id\": 1,\n    \"status\": \"it's\",\n    \"total\": 26.75\n  },\n  {\n    \"id\": 2,\n    \"status\": \"a|b\\nc\",\n    \"total\": null\n  }\n]");
        let (c, r) = (cols(), Vec::new());
        assert_eq!(format(Fmt::Json, &Sheet { rows: &r, cols: &c, table: "", pk: &[], bs: true }, true), "[]");
    }

    #[test]
    fn markdown_aligns_numbers_and_escapes() {
        let md = out(Fmt::Markdown, true);
        let lines: Vec<&str> = md.lines().collect();
        assert_eq!(lines[0], "| id  | status    | total |");
        assert_eq!(lines[1], "| --: | --------- | ----: |");
        assert_eq!(lines[2], "|   1 | it's      | 26.75 |");
        assert_eq!(lines[3], "|   2 | a\\|b<br>c |       |");
        assert!(out(Fmt::Markdown, false).starts_with("|     |           |       |\n"));
    }

    #[test]
    fn html_escapes_and_has_thead() {
        let c = [CopyCol { name: "a<b".into(), src: "a<b".into(), numeric: false }];
        let r = [doc! { "a<b": "x & \"y\" <z>" }];
        let s = Sheet { rows: &r, cols: &c, table: "", pk: &[], bs: true };
        assert_eq!(format(Fmt::Html, &s, true), "<table>\n  <thead><tr><th>a&lt;b</th></tr></thead>\n  <tbody>\n    <tr><td>x &amp; &quot;y&quot; &lt;z&gt;</td></tr>\n  </tbody>\n</table>");
        assert!(!format(Fmt::Html, &s, false).contains("<thead>"));
    }

    #[test]
    fn values_one_per_line_and_mongo_types() {
        assert_eq!(out(Fmt::Values, true), "1\nit's\n26.75\n2\na|b\nc\n");
        let c = [CopyCol { name: "a.b".into(), src: "a.b".into(), numeric: false }, CopyCol { name: "bin".into(), src: "bin".into(), numeric: false }];
        let r = [doc! { "a": { "b": 5 }, "bin": Bson::Binary(mongodb::bson::Binary { subtype: mongodb::bson::spec::BinarySubtype::Generic, bytes: vec![1, 255] }) }];
        assert_eq!(format(Fmt::Values, &Sheet { rows: &r, cols: &c, table: "", pk: &[], bs: true }, true), "5\n0x01FF");
    }
}
