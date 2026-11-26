//! Client-side sort of one Custom Query result page. The server is never asked again: only the rows already fetched move.

use super::value::Kind;
use super::ResultSet;
use mongodb::bson::raw::RawBsonRef;
use std::cmp::Ordering;

/// Sorts the rows of `set` by `keys` (column name and whether it is descending), first key first. Ties keep the order the
/// server returned, which `set.seq` remembers, so removing every key restores it. Returns the new position of each row by
/// its position before the call, for selections that follow their rows. A key that names no column is ignored.
pub fn sort_rows(set: &mut ResultSet, keys: &[(String, bool)]) -> Vec<usize> {
    let n = set.rows.len();
    if set.seq.len() != n {
        set.seq = (0..n).collect();
    }
    let by: Vec<(&str, bool, Kind)> = keys.iter().filter_map(|(c, desc)| set.cols.iter().find(|x| x.name == *c).map(|x| (x.name.as_str(), *desc, x.kind))).collect();
    // Looked up once per row: a cell lookup scans the raw document, which a comparison would repeat n log n times.
    let cells: Vec<Vec<Option<RawBsonRef>>> = set.rows.iter().map(|r| by.iter().map(|&(name, ..)| cell(r, name)).collect()).collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&x, &y| {
        for (k, &(_, desc, kind)) in by.iter().enumerate() {
            let (a, b) = (cells[x][k], cells[y][k]);
            // A missing value goes last in both directions, so the values a person looks for stay on top.
            let o = match (a, b) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => return Ordering::Greater,
                (Some(_), None) => return Ordering::Less,
                (Some(a), Some(b)) => {
                    let o = cmp_values(a, b, kind);
                    if desc { o.reverse() } else { o }
                }
            };
            if o != Ordering::Equal {
                return o;
            }
        }
        set.seq[x].cmp(&set.seq[y])
    });
    drop(cells);
    let mut old: Vec<Option<crate::db::Row>> = std::mem::take(&mut set.rows).into_iter().map(Some).collect();
    let seq = std::mem::take(&mut set.seq);
    let mut moved = vec![0; n];
    for (new, &was) in order.iter().enumerate() {
        moved[was] = new;
    }
    set.rows = order.iter().filter_map(|&i| old[i].take()).collect();
    set.seq = order.iter().map(|&i| seq[i]).collect();
    set.sort = by.iter().map(|&(c, d, _)| (c.to_string(), d)).collect();
    // The widths were measured on the first rows of the old order.
    set.widest = Default::default();
    moved
}

/// The top-level value of a column, never a path: SQL column names can hold a dot. NULL is `None`.
fn cell<'a>(r: &'a crate::db::Row, name: &str) -> Option<RawBsonRef<'a>> {
    r.raw().get(name).ok().flatten().filter(|v| !matches!(v, RawBsonRef::Null))
}

fn is_number(k: Kind) -> bool {
    matches!(k, Kind::Int | Kind::UInt | Kind::Float | Kind::Decimal)
}

/// Where a value sits when a column mixes types: numbers, then text, then the rest.
fn rank(v: &RawBsonRef) -> u8 {
    match v {
        RawBsonRef::Int32(_) | RawBsonRef::Int64(_) | RawBsonRef::Double(_) => 0,
        RawBsonRef::String(_) => 1,
        _ => 2,
    }
}

/// Orders two non-null values. Numbers compare as numbers whatever their width, and DECIMAL and big integers, which rows
/// keep as text to stay exact, compare as numbers when the column is numeric. Text ignores case first.
pub fn cmp_values(a: RawBsonRef, b: RawBsonRef, kind: Kind) -> Ordering {
    use RawBsonRef::*;
    match (a, b) {
        (Int32(x), Int32(y)) => x.cmp(&y),
        (Int32(x), Int64(y)) => i64::from(x).cmp(&y),
        (Int64(x), Int32(y)) => x.cmp(&i64::from(y)),
        (Int64(x), Int64(y)) => x.cmp(&y),
        (a @ (Int32(_) | Int64(_) | Double(_)), b @ (Int32(_) | Int64(_) | Double(_))) => as_f64(&a).total_cmp(&as_f64(&b)),
        (Int32(_) | Int64(_) | Double(_), String(s)) if is_number(kind) => cmp_values_num_text(&a, s, false),
        (String(s), Int32(_) | Int64(_) | Double(_)) if is_number(kind) => cmp_values_num_text(&b, s, true),
        (String(x), String(y)) if is_number(kind) => cmp_decimal(x, y).unwrap_or_else(|| cmp_text(x, y)),
        (String(x), String(y)) => cmp_text(x, y),
        (Boolean(x), Boolean(y)) => x.cmp(&y),
        (DateTime(x), DateTime(y)) => x.timestamp_millis().cmp(&y.timestamp_millis()),
        (Binary(x), Binary(y)) => x.bytes.cmp(y.bytes),
        (a, b) => rank(&a).cmp(&rank(&b)),
    }
}

fn as_f64(v: &RawBsonRef) -> f64 {
    match v {
        RawBsonRef::Int32(n) => f64::from(*n),
        RawBsonRef::Int64(n) => *n as f64,
        RawBsonRef::Double(f) => *f,
        _ => f64::NAN,
    }
}

/// A number against text of a numeric column; `flip` when the text came first.
fn cmp_values_num_text(num: &RawBsonRef, text: &str, flip: bool) -> Ordering {
    let o = match text.trim().parse::<f64>() {
        Ok(t) => as_f64(num).total_cmp(&t),
        // Text that is no number sorts after the numbers.
        Err(_) => Ordering::Less,
    };
    if flip { o.reverse() } else { o }
}

fn cmp_text(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase)).then_with(|| a.cmp(b))
}

/// Exact comparison of two plain decimal numbers such as `-12.50` and `3`, which f64 would round past 15 digits.
/// `None` when either is not that form (an exponent, a word), so the caller falls back.
fn cmp_decimal(a: &str, b: &str) -> Option<Ordering> {
    fn split(s: &str) -> Option<(bool, &str, &str)> {
        let s = s.trim();
        let (neg, s) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (int, frac) = s.split_once('.').unwrap_or((s, ""));
        let digits = |t: &str| t.bytes().all(|c| c.is_ascii_digit());
        (!s.is_empty() && digits(int) && digits(frac) && !(int.is_empty() && frac.is_empty())).then_some((neg, int.trim_start_matches('0'), frac.trim_end_matches('0')))
    }
    let ((na, ia, fa), (nb, ib, fb)) = (split(a)?, split(b)?);
    let zero = |i: &str, f: &str| i.is_empty() && f.is_empty();
    let (za, zb) = (zero(ia, fa), zero(ib, fb));
    // Negative zero is zero.
    let (na, nb) = (na && !za, nb && !zb);
    if na != nb {
        return Some(if na { Ordering::Less } else { Ordering::Greater });
    }
    let mag = ia.len().cmp(&ib.len()).then_with(|| ia.cmp(ib)).then_with(|| fa.cmp(fb));
    Some(if na { mag.reverse() } else { mag })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::sql::Col;
    use mongodb::bson::doc;

    fn set(kind: Kind, vals: Vec<mongodb::bson::Bson>) -> ResultSet {
        let mut s = ResultSet::new("SELECT v FROM t");
        s.cols = vec![Col::plain("v", "X", kind, true)];
        s.rows = vals.into_iter().map(|v| crate::db::Row::from_doc(&doc! { "v": v }).unwrap()).collect();
        s
    }

    fn col(s: &ResultSet) -> Vec<String> {
        s.rows.iter().map(|r| r.raw().get("v").ok().flatten().map_or("NULL".to_string(), |v| match v {
            RawBsonRef::Int32(n) => n.to_string(),
            RawBsonRef::Int64(n) => n.to_string(),
            RawBsonRef::Double(f) => f.to_string(),
            RawBsonRef::String(t) => t.to_string(),
            _ => "NULL".to_string(),
        })).collect()
    }

    #[test]
    fn nulls_go_last_in_both_directions() {
        use mongodb::bson::Bson::*;
        let mut s = set(Kind::Int, vec![Null, Int32(2), Int64(1), Null, Int32(3)]);
        sort_rows(&mut s, &[("v".into(), false)]);
        assert_eq!(col(&s), ["1", "2", "3", "NULL", "NULL"]);
        sort_rows(&mut s, &[("v".into(), true)]);
        assert_eq!(col(&s), ["3", "2", "1", "NULL", "NULL"]);
    }

    #[test]
    fn mixed_number_widths_compare_as_numbers() {
        use mongodb::bson::Bson::*;
        let mut s = set(Kind::Float, vec![Double(4.5), Int32(5), Int64(3), Int32(-1)]);
        sort_rows(&mut s, &[("v".into(), false)]);
        assert_eq!(col(&s), ["-1", "3", "4.5", "5"]);
    }

    #[test]
    fn decimal_text_sorts_by_value_not_by_characters() {
        use mongodb::bson::Bson::String as S;
        let mut s = set(Kind::Decimal, vec![S("10.5".into()), S("9".into()), S("-2.25".into()), S("10.50".into()), S("123456789012345678.1".into()), S("123456789012345678.2".into())]);
        sort_rows(&mut s, &[("v".into(), false)]);
        assert_eq!(col(&s), ["-2.25", "9", "10.5", "10.50", "123456789012345678.1", "123456789012345678.2"]);
    }

    #[test]
    fn text_ignores_case_and_ties_keep_the_server_order() {
        use mongodb::bson::Bson::String as S;
        let mut s = set(Kind::Text, vec![S("b".into()), S("A".into()), S("a".into()), S("B".into())]);
        sort_rows(&mut s, &[("v".into(), false)]);
        assert_eq!(col(&s), ["A", "a", "B", "b"]);
    }

    #[test]
    fn no_keys_restores_the_original_order_and_selections_follow_rows() {
        use mongodb::bson::Bson::Int32;
        let mut s = set(Kind::Int, vec![Int32(3), Int32(1), Int32(2)]);
        let moved = sort_rows(&mut s, &[("v".into(), false)]);
        // The row that was first (3) is now last.
        assert_eq!(moved, [2, 0, 1]);
        sort_rows(&mut s, &[]);
        assert_eq!(col(&s), ["3", "1", "2"]);
        assert!(s.sort.is_empty());
    }

    #[test]
    fn a_second_key_breaks_ties_of_the_first() {
        let mut s = ResultSet::new("SELECT a, b FROM t");
        s.cols = vec![Col::plain("a", "X", Kind::Int, false), Col::plain("b", "X", Kind::Text, false)];
        s.rows = [(1, "y"), (2, "x"), (1, "x")].iter().map(|&(a, b)| crate::db::Row::from_doc(&doc! { "a": a, "b": b }).unwrap()).collect();
        sort_rows(&mut s, &[("a".into(), false), ("b".into(), true)]);
        let got: Vec<(i32, String)> = s.rows.iter().map(|r| (r.raw().get_i32("a").unwrap(), r.raw().get_str("b").unwrap().to_string())).collect();
        assert_eq!(got, [(1, "y".to_string()), (1, "x".to_string()), (2, "x".to_string())]);
    }

    #[test]
    fn a_dotted_column_name_is_read_whole() {
        let mut s = ResultSet::new("SELECT 1");
        s.cols = vec![Col::plain("a.b", "X", Kind::Int, false)];
        s.rows = [2, 1].iter().map(|&n| crate::db::Row::from_doc(&doc! { "a.b": n }).unwrap()).collect();
        sort_rows(&mut s, &[("a.b".into(), false)]);
        assert_eq!(s.rows[0].raw().get_i32("a.b").unwrap(), 1);
    }
}
