//! Import parsing (JSON, JSON lines, CSV) and export bodies.

use super::*;

/// File contents for exported documents: CSV, TSV, or a pretty JSON array for anything else.
pub fn export_body(docs: &[Document], ext: &str) -> Res<String> {
    Ok(match ext {
        "csv" => to_table(docs, ','),
        "tsv" => to_table(docs, '\t'),
        _ => serde_json::to_string_pretty(&docs.iter().map(|d| Bson::Document(d.clone()).into_relaxed_extjson()).collect::<Vec<_>>()).map_err(e)?,
    })
}

/// CSV or TSV with every top-level field seen in any document, RFC 4180 quoting.
pub fn to_table(docs: &[Document], sep: char) -> String {
    let mut cols: Vec<String> = Vec::new();
    for d in docs {
        for k in d.keys() {
            if !cols.contains(k) {
                cols.push(k.clone());
            }
        }
    }
    let cell = |s: String| -> String {
        if sep == '\t' {
            s.replace(['\t', '\n', '\r'], " ")
        } else if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s
        }
    };
    let mut out = cols.iter().map(|c| cell(c.clone())).collect::<Vec<_>>().join(&sep.to_string());
    for d in docs {
        out.push('\n');
        let row: Vec<String> = cols
            .iter()
            .map(|c| {
                cell(match d.get(c) {
                    None | Some(Bson::Null) => String::new(),
                    Some(Bson::String(s)) => s.clone(),
                    Some(Bson::ObjectId(o)) => o.to_hex(),
                    Some(Bson::DateTime(t)) => t.try_to_rfc3339_string().unwrap_or_default(),
                    Some(other) => compact(other),
                })
            })
            .collect();
        out.push_str(&row.join(&sep.to_string()));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Default)]
pub enum ImportKind {
    #[default]
    Json,
    JsonLines,
    Csv,
}

pub struct CsvOptions {
    /// None = detect among , tab | ; (the original's delimitersToGuess).
    pub separator: Option<char>,
    pub header: bool,
    pub camel_case: bool,
}

/// Parses pasted or loaded text into documents, following the original's import rules.
pub fn parse_import(text: &str, kind: ImportKind, csv: &CsvOptions) -> Res<Vec<Document>> {
    match kind {
        ImportKind::Json => match parse_value(text)? {
            Bson::Array(a) => a
                .into_iter()
                .enumerate()
                .map(|(i, x)| match x {
                    Bson::Document(d) => Ok(d),
                    _ => Err(format!("Item {} is not a document", i + 1)),
                })
                .collect(),
            Bson::Document(d) => Ok(vec![d]),
            _ => Err("Expected a JSON array of documents or one document".into()),
        },
        ImportKind::JsonLines => text
            .lines()
            .enumerate()
            .filter(|(_, l)| !l.trim().is_empty())
            .map(|(i, l)| parse_doc(l).map_err(|err| format!("Line {}: {err}", i + 1)))
            .collect(),
        ImportKind::Csv => {
            let sep = csv.separator.unwrap_or_else(|| guess_separator(text));
            let mut rows = csv_rows(text, sep);
            if rows.is_empty() {
                return Ok(Vec::new());
            }
            let names: Vec<String> = if csv.header {
                rows.remove(0).into_iter().map(|h| if csv.camel_case { camel_case(&h) } else { h.trim().to_string() }).collect()
            } else {
                (1..=rows.iter().map(|r| r.len()).max().unwrap_or(0)).map(|i| format!("column{i}")).collect()
            };
            Ok(rows
                .into_iter()
                .map(|r| {
                    let mut d = Document::new();
                    for (i, cell) in r.into_iter().enumerate() {
                        let name = names.get(i).cloned().unwrap_or_else(|| format!("column{}", i + 1));
                        d.insert(name, csv_value(&cell));
                    }
                    d
                })
                .collect())
        }
    }
}

pub(crate) fn guess_separator(text: &str) -> char {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default();
    [',', '\t', '|', ';'].into_iter().max_by_key(|c| first.matches(*c).count()).unwrap_or(',')
}

/// RFC 4180 rows: quotes with "" escapes, separators and newlines inside quotes, empty lines skipped.
pub(crate) fn csv_rows(text: &str, sep: char) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut cell) = (Vec::new(), Vec::new(), String::new());
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (true, c) => cell.push(c),
            (false, '"') if cell.is_empty() => quoted = true,
            (false, c) if c == sep => row.push(std::mem::take(&mut cell)),
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut cell));
                if !(row.len() == 1 && row[0].is_empty()) {
                    rows.push(std::mem::take(&mut row));
                }
                row.clear();
            }
            (false, c) => cell.push(c),
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }
    rows
}

/// Papa Parse's dynamicTyping, as the original imports CSV: booleans, numbers and ISO dates get
/// their type; an empty cell becomes null.
pub(crate) fn csv_value(cell: &str) -> Bson {
    let t = cell.trim();
    if t.is_empty() {
        return Bson::Null;
    }
    if t.eq_ignore_ascii_case("true") {
        return Bson::Boolean(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Bson::Boolean(false);
    }
    let numeric = t.chars().enumerate().all(|(i, c)| c.is_ascii_digit() || (i == 0 && c == '-') || c == '.' || c == 'e' || c == 'E' || c == '+')
        && t.chars().any(|c| c.is_ascii_digit())
        && !(t.len() > 1 && t.starts_with('0') && !t.starts_with("0."));
    if numeric {
        if let Ok(n) = t.parse::<i64>() {
            return i32::try_from(n).map(Bson::Int32).unwrap_or(Bson::Int64(n));
        }
        if let Ok(f) = t.parse::<f64>() {
            return Bson::Double(f);
        }
    }
    if t.len() >= 20 && t.as_bytes()[4] == b'-' && t.contains('T') {
        if let Ok(d) = mongodb::bson::DateTime::parse_rfc3339_str(t) {
            return Bson::DateTime(d);
        }
    }
    Bson::String(cell.to_string())
}

/// lodash camelCase for header names ("First Name" -> firstName).
pub(crate) fn camel_case(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .enumerate()
        .map(|(i, p)| {
            let lower = p.to_lowercase();
            if i == 0 {
                return lower;
            }
            let mut chars = lower.chars();
            chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camel_case_headers() {
        assert_eq!(camel_case("First Name"), "firstName");
        assert_eq!(camel_case("Prénom Élève"), "prénomÉlève");
        assert_eq!(camel_case("über straße"), "überStraße");
    }
}
