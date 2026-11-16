//! Rows written as an Excel workbook: one sheet holding a spreadsheet table with a frozen, filterable header, banded
//! rows and no gridlines, and an optional About sheet saying where the rows came from. Cells keep their types: numbers
//! are numbers, dates and times are dates, booleans are TRUE or FALSE and NULL is an empty cell. The sheet is written in
//! constant memory mode, so a streamed result of any size keeps only the current row in memory.

use super::sql::value::Kind;
use mongodb::bson::{Bson, Document};
use rust_xlsxwriter::{Color, ConditionalFormatFormula, ExcelDateTime, Format, FormatAlign, FormatBorder, Table, TableColumn, TableStyle, Workbook, Worksheet};

/// One column: the key of its value in a row document, and the SQL column's kind when there is one. Without a kind each
/// cell is written by its BSON type, which is all a MongoDB document says.
pub struct XCol {
    pub name: String,
    pub kind: Option<Kind>,
}

/// What a streamed export needs besides its columns, which only the result brings.
#[derive(Clone, Default)]
pub struct Opts {
    pub sheet: String,
    /// The About sheet's lines, when it is wanted.
    pub about: Option<Vec<(String, String)>>,
}

/// Excel holds this many rows in a sheet, the header included.
const MAX_ROWS: u32 = 1_048_576;
/// Excel refuses a longer cell.
const MAX_CELL: usize = 32_767;
/// Integers past this lose digits as an Excel number, so they are written as text.
const EXACT: i64 = 1 << 53;
/// The widest a column is made, in characters.
const MAX_W: usize = 50;
const MONO: &str = "Consolas";
const INK: u32 = 0x1C2A3D;
const HEAD_FILL: u32 = 0xE6ECF4;
const HEAD_RULE: u32 = 0x9FB0C8;
const BAND: u32 = 0xF5F7FA;

struct Formats {
    header: Format,
    int: Format,
    /// An id or a year: no thousands separator.
    plain_int: Format,
    general: Format,
    /// DECIMAL by its number of decimals, 0 to 10.
    dec: Vec<Format>,
    date: Format,
    datetime: Format,
    time: Format,
    text: Format,
    mono: Format,
}

impl Formats {
    fn new() -> Formats {
        Formats {
            header: Format::new().set_bold().set_font_color(Color::RGB(INK)).set_background_color(Color::RGB(HEAD_FILL)).set_border_bottom(FormatBorder::Thin).set_border_bottom_color(Color::RGB(HEAD_RULE)),
            int: Format::new().set_num_format("#,##0"),
            plain_int: Format::new().set_num_format("0"),
            general: Format::new(),
            dec: (0..=10).map(|n| Format::new().set_num_format(if n == 0 { "#,##0".to_string() } else { format!("#,##0.{}", "0".repeat(n)) })).collect(),
            date: Format::new().set_num_format("yyyy-mm-dd").set_align(FormatAlign::Left),
            datetime: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss").set_align(FormatAlign::Left),
            time: Format::new().set_num_format("hh:mm:ss").set_align(FormatAlign::Left),
            text: Format::new(),
            mono: Format::new().set_font_name(MONO),
        }
    }
}

pub struct Book {
    wb: Workbook,
    cols: Vec<XCol>,
    /// Columns that hold ids, written without a thousands separator.
    plain: Vec<bool>,
    widths: Vec<usize>,
    /// The next row to write; the header is row 0.
    row: u32,
    f: Formats,
    about: Option<Vec<(String, String)>>,
}

fn err(e: rust_xlsxwriter::XlsxError) -> String {
    format!("Could not write the workbook: {e}")
}

/// A column that holds an id or a year, which read wrong with a thousands separator.
fn id_like(name: &str, kind: Option<Kind>) -> bool {
    let n = name.to_ascii_lowercase();
    kind == Some(Kind::Year) || n == "id" || n == "_id" || n.ends_with("_id") || n.ends_with("id") && name.ends_with("Id") || n == "year" || n.ends_with("_year")
}

/// A sheet name Excel takes: at most 31 characters, none of `[]:*?/\`, no apostrophe at either end.
pub fn sheet_name(name: &str) -> String {
    let s: String = name.chars().map(|c| if "[]:*?/\\".contains(c) || c.is_control() { ' ' } else { c }).collect();
    let s: String = s.trim().trim_matches('\'').trim().chars().take(31).collect();
    if s.is_empty() { "Rows".into() } else { s }
}

/// The digits a number keeps, to tell whether Excel's 15 significant digits hold it exactly.
fn significant(s: &str) -> usize {
    s.trim_start_matches(['-', '+']).trim_start_matches(['0', '.']).bytes().filter(u8::is_ascii_digit).count()
}

/// 1284 as "1,284".
fn group3(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn digits(n: u32, s: &str) -> Option<u32> {
    (s.len() == n as usize && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// `HH:MM[:SS[.fff]]` as hour, minute and seconds.
fn hms(s: &str) -> Option<(u16, u8, f64)> {
    let mut it = s.splitn(3, ':');
    let h = digits(2, it.next()?)?;
    let m = digits(2, it.next()?)?;
    let sec = match it.next() {
        Some(x) => x.parse::<f64>().ok().filter(|v| (0.0..60.0).contains(v))?,
        None => 0.0,
    };
    (h < 24 && m < 60).then_some((h as u16, m as u8, sec))
}

/// A date, or a date and time, as the server writes it: `YYYY-MM-DD`, then optionally ` HH:MM:SS[.fff]` or the same after
/// a `T`, and a time zone, which is dropped so the cell shows the time the grid shows. Before 1900 Excel has no dates,
/// and a zero date or `infinity` is not one, so those stay text. The bool is true when there is a time.
fn parse_date(s: &str) -> Option<(ExcelDateTime, bool)> {
    let s = s.trim();
    let (y, m, d) = (digits(4, s.get(0..4)?)?, digits(2, s.get(5..7)?)?, digits(2, s.get(8..10)?)?);
    if s.get(4..5)? != "-" || s.get(7..8)? != "-" || y < 1900 {
        return None;
    }
    let date = ExcelDateTime::from_ymd(y as u16, m as u8, d as u8).ok()?;
    let rest = &s[10..];
    if rest.is_empty() {
        return Some((date, false));
    }
    let rest = rest.strip_prefix([' ', 'T'])?;
    // The time ends where a zone starts: Z, +hh[:mm] or -hh[:mm].
    let end = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
    let (h, mi, sec) = hms(rest[..end].trim())?;
    Some((date.and_hms(h, mi, sec).ok()?, true))
}

impl Book {
    pub fn new(sheet: &str, cols: Vec<XCol>, about: Option<Vec<(String, String)>>) -> Result<Book, String> {
        let mut wb = Workbook::new();
        let f = Formats::new();
        {
            let ws = wb.add_worksheet_with_constant_memory();
            ws.set_name(sheet_name(sheet)).map_err(err)?;
            for (i, c) in cols.iter().enumerate() {
                ws.write_string_with_format(0, i as u16, &c.name, &f.header).map_err(err)?;
            }
        }
        let plain = cols.iter().map(|c| id_like(&c.name, c.kind)).collect();
        // A header also carries the filter button.
        let widths = cols.iter().map(|c| c.name.chars().count() + 3).collect();
        Ok(Book { wb, cols, plain, widths, row: 1, f, about })
    }

    fn sheet(&mut self) -> Result<&mut Worksheet, String> {
        self.wb.worksheet_from_index(0).map_err(err)
    }

    pub fn rows(&mut self, rows: &[Document]) -> Result<(), String> {
        if self.row as usize + rows.len() > MAX_ROWS as usize {
            return Err("The result has more rows than an Excel sheet holds (1,048,576). Export it as CSV instead.".into());
        }
        for d in rows {
            for i in 0..self.cols.len() {
                // A SQL column can hold a dot in its name, so the exact key wins over a path.
                let name = &self.cols[i].name;
                let v = d.get(name).or_else(|| super::get_path(d, name)).cloned();
                let w = self.cell(i, v.as_ref())?;
                self.widths[i] = self.widths[i].max(w);
            }
            self.row += 1;
        }
        Ok(())
    }

    fn text(&mut self, c: usize, s: &str, mono: bool) -> Result<usize, String> {
        let r = self.row;
        let s: String = s.chars().take(MAX_CELL).collect();
        let n = s.chars().count();
        let f = if mono { self.f.mono.clone() } else { self.f.text.clone() };
        self.sheet()?.write_string_with_format(r, c as u16, s, &f).map_err(err)?;
        Ok(n)
    }

    fn number(&mut self, c: usize, v: f64, f: Format, w: usize) -> Result<usize, String> {
        let r = self.row;
        self.sheet()?.write_number_with_format(r, c as u16, v, &f).map_err(err)?;
        Ok(w)
    }

    fn int(&mut self, c: usize, n: i64) -> Result<usize, String> {
        if n.abs() > EXACT {
            return self.text(c, &n.to_string(), false);
        }
        let plain = self.plain[c];
        let len = n.to_string().len();
        let f = if plain { self.f.plain_int.clone() } else { self.f.int.clone() };
        self.number(c, n as f64, f, if plain { len } else { len + len / 3 })
    }

    /// DECIMAL text: a number with as many decimals as the value has, or text when Excel would lose digits of it.
    fn decimal(&mut self, c: usize, s: &str) -> Result<usize, String> {
        let s = s.trim();
        match s.parse::<f64>() {
            Ok(v) if v.is_finite() && significant(s) <= 15 => {
                let scale = s.split_once('.').map_or(0, |(_, d)| d.len()).min(10);
                let f = self.f.dec[scale].clone();
                self.number(c, v, f, s.len() + s.len() / 3)
            }
            _ => self.text(c, s, false),
        }
    }

    fn date(&mut self, c: usize, s: &str) -> Result<usize, String> {
        match parse_date(s) {
            Some((dt, time)) => {
                let r = self.row;
                let f = if time { self.f.datetime.clone() } else { self.f.date.clone() };
                self.sheet()?.write_datetime_with_format(r, c as u16, &dt, &f).map_err(err)?;
                Ok(if time { 19 } else { 10 })
            }
            None => self.text(c, s, false),
        }
    }

    fn time(&mut self, c: usize, s: &str) -> Result<usize, String> {
        match hms(s.trim()).and_then(|(h, m, sec)| ExcelDateTime::from_hms(h, m, sec).ok()) {
            Some(t) => {
                let (r, f) = (self.row, self.f.time.clone());
                self.sheet()?.write_datetime_with_format(r, c as u16, &t, &f).map_err(err)?;
                Ok(8)
            }
            // A MySQL TIME can be a duration past 24 hours or negative, which is not a time of day.
            None => self.text(c, s, false),
        }
    }

    /// Writes one cell and returns how many characters wide it reads.
    fn cell(&mut self, c: usize, v: Option<&Bson>) -> Result<usize, String> {
        let kind = self.cols[c].kind;
        match v {
            None | Some(Bson::Null | Bson::Undefined) => Ok(0),
            Some(Bson::Boolean(b)) => {
                let r = self.row;
                let f = self.f.general.clone().set_align(FormatAlign::Center);
                self.sheet()?.write_boolean_with_format(r, c as u16, *b, &f).map_err(err)?;
                Ok(5)
            }
            Some(Bson::Int32(n)) => self.int(c, i64::from(*n)),
            Some(Bson::Int64(n)) => self.int(c, *n),
            Some(Bson::Double(v)) if v.is_finite() => {
                let f = self.f.general.clone();
                self.number(c, *v, f, v.to_string().len().min(15))
            }
            Some(Bson::Decimal128(d)) => self.decimal(c, &d.to_string()),
            Some(Bson::DateTime(t)) => self.date(c, &t.try_to_rfc3339_string().unwrap_or_default()),
            Some(Bson::ObjectId(o)) => self.text(c, &o.to_hex(), true),
            Some(Bson::Binary(b)) => self.text(c, &format!("0x{}", super::sql::value::hex_digits(&b.bytes, true)), true),
            Some(Bson::String(s)) => match kind {
                Some(Kind::Decimal) => self.decimal(c, s),
                Some(Kind::Date | Kind::DateTime) => self.date(c, s),
                Some(Kind::Time) => self.time(c, s),
                Some(Kind::Json | Kind::Uuid | Kind::Array | Kind::Inet | Kind::Range | Kind::Interval | Kind::Bit | Kind::Geometry) => self.text(c, s, true),
                _ => self.text(c, s, false),
            },
            Some(other) => self.text(c, &super::compact(other), true),
        }
    }

    /// The finished file. `stmt` is the statement the rows came from, for the About sheet.
    pub fn finish(mut self, stmt: Option<&str>) -> Result<Vec<u8>, String> {
        let n = self.cols.len();
        let rows = self.row - 1;
        let widths = std::mem::take(&mut self.widths);
        let headers: Vec<TableColumn> = self.cols.iter().map(|c| TableColumn::new().set_header(&c.name)).collect();
        {
            let ws = self.sheet()?;
            if n > 0 {
                // A table needs a data row, so an empty result gets an empty one.
                let last = rows.max(1);
                let last_col = (n - 1) as u16;
                let table = Table::new().set_style(TableStyle::None).set_columns(&headers);
                // ponytail: in constant memory mode the table's own header write lands on a flushed row, which the
                // crate skips with a line on stderr; the header cells written first stay.
                if ws.add_table(0, 0, last, last_col, &table).is_err() {
                    // Excel refuses two headers that differ only in case; the filter buttons still work without a table.
                    ws.autofilter(0, 0, last, last_col).map_err(err)?;
                }
                let band = ConditionalFormatFormula::new().set_rule("=MOD(ROW(),2)=1").set_format(Format::new().set_background_color(Color::RGB(BAND)));
                ws.add_conditional_format(1, 0, last, last_col, &band).map_err(err)?;
                for (i, w) in widths.iter().enumerate() {
                    ws.set_column_width(i as u16, (*w).clamp(6, MAX_W) as f64 + 2.0).map_err(err)?;
                }
            }
            ws.set_freeze_panes(1, 0).map_err(err)?;
            ws.set_screen_gridlines(false);
        }
        if let Some(lines) = self.about.take() {
            self.about_sheet(lines, rows, stmt)?;
        }
        self.wb.save_to_buffer().map_err(err)
    }

    fn about_sheet(&mut self, lines: Vec<(String, String)>, rows: u32, stmt: Option<&str>) -> Result<(), String> {
        let ws = self.wb.add_worksheet();
        ws.set_name("About").map_err(err)?;
        ws.set_screen_gridlines(false);
        ws.set_column_width(0, 16).map_err(err)?;
        ws.set_column_width(1, 100).map_err(err)?;
        let key = Format::new().set_font_color(Color::RGB(0x6B6F76)).set_align(FormatAlign::Top);
        ws.write_string_with_format(0, 0, "About this export", &Format::new().set_bold().set_font_size(14)).map_err(err)?;
        ws.write_string_with_format(1, 0, "Written by Quarry.", &key).map_err(err)?;
        let plain = Format::new().set_align(FormatAlign::Left);
        let mut lines = lines;
        lines.push(("Rows".into(), group3(rows)));
        let mut r = 3;
        for (k, v) in &lines {
            ws.write_string_with_format(r, 0, k, &key).map_err(err)?;
            ws.write_string_with_format(r, 1, v.chars().take(MAX_CELL).collect::<String>(), &plain).map_err(err)?;
            r += 1;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        if let Ok(t) = ExcelDateTime::from_timestamp(now) {
            ws.write_string_with_format(r, 0, "Exported", &key).map_err(err)?;
            ws.write_datetime_with_format(r, 1, &t, &Format::new().set_num_format("yyyy-mm-dd hh:mm \"UTC\"").set_align(FormatAlign::Left)).map_err(err)?;
            r += 1;
        }
        if let Some(s) = stmt.filter(|s| !s.trim().is_empty()) {
            ws.write_string_with_format(r, 0, "Statement", &key).map_err(err)?;
            ws.write_string_with_format(r, 1, s.trim().chars().take(MAX_CELL).collect::<String>(), &Format::new().set_font_name(MONO).set_text_wrap().set_align(FormatAlign::Top)).map_err(err)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn dates_parse_as_the_server_writes_them_and_others_stay_text() {
        assert!(matches!(parse_date("2026-11-14"), Some((_, false))));
        assert!(matches!(parse_date("2026-11-14 09:32:00"), Some((_, true))));
        assert!(matches!(parse_date("2026-11-14T09:32:00.123+01:00"), Some((_, true))));
        assert!(matches!(parse_date("2026-11-14 09:32:00+01"), Some((_, true))));
        for bad in ["0000-00-00", "1899-12-31", "infinity", "2026-13-01", "2026-11-14 25:00:00", "14/11/2026"] {
            assert!(parse_date(bad).is_none(), "{bad}");
        }
        assert_eq!(hms("838:59:59"), None);
        assert_eq!(significant("-0.00012345"), 5);
        assert_eq!(significant("12345678901234567.5"), 18);
        assert_eq!(sheet_name("a/b: [c]"), "a b   c");
        assert_eq!(sheet_name("'"), "Rows");
    }

    /// The parts of the file as text, by name.
    fn parts(bytes: &[u8]) -> std::collections::HashMap<String, String> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        (0..z.len())
            .map(|i| {
                let mut f = z.by_index(i).unwrap();
                let mut s = String::new();
                std::io::Read::read_to_string(&mut f, &mut s).unwrap_or_default();
                (f.name().to_string(), s)
            })
            .collect()
    }

    #[test]
    fn cells_keep_their_types_in_a_banded_table() {
        let cols = [("id", Some(Kind::Int)), ("total", Some(Kind::Decimal)), ("at", Some(Kind::DateTime)), ("paid", None), ("note", Some(Kind::Text)), ("big", Some(Kind::Decimal))];
        let cols = cols.into_iter().map(|(n, k)| XCol { name: n.into(), kind: k }).collect();
        let mut b = Book::new("orders", cols, Some(vec![("Table".into(), "orders".into())])).unwrap();
        b.rows(&[doc! { "id": 10421_i64, "total": "1284.50", "at": "2026-11-14 09:32:00", "paid": true, "note": Bson::Null, "big": "12345678901234567.5" }, doc! { "id": 10422_i64, "total": "89.90", "at": "0000-00-00 00:00:00", "paid": false, "note": "door" }]).unwrap();
        let p = parts(&b.finish(Some("SELECT * FROM orders")).unwrap());
        let sheet = &p["xl/worksheets/sheet1.xml"];
        // Numbers and dates are numeric cells, booleans are b cells, text is inline text.
        assert!(sheet.contains("<v>10421</v>") && sheet.contains("<v>1284.5</v>"), "{sheet}");
        assert!(sheet.contains("t=\"b\""), "{sheet}");
        assert!(sheet.contains("12345678901234567.5"), "a DECIMAL past 15 digits is kept as text");
        assert!(sheet.contains("0000-00-00 00:00:00"), "a zero date stays text");
        assert!(sheet.contains("<pane ") && sheet.contains("showGridLines=\"0\""), "{sheet}");
        assert!(sheet.contains("<conditionalFormatting"), "{sheet}");
        assert!(p.contains_key("xl/tables/table1.xml"));
        assert!(p.keys().any(|k| k == "xl/worksheets/sheet2.xml"), "the About sheet");
        let styles = &p["xl/styles.xml"];
        assert!(styles.contains("#,##0.00") && styles.contains("yyyy-mm-dd hh:mm:ss"), "{styles}");
    }
}
