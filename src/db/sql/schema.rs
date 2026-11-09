//! What autocomplete knows about one database: its tables, views and columns, read in one go.

use super::value::Kind;

#[derive(Clone, Debug, PartialEq)]
pub struct SchemaCol {
    pub name: String,
    /// The server's COLUMN_TYPE, such as "varchar(20)" or "enum('a','b')".
    pub ty: String,
    pub kind: Kind,
    /// "PRI", "UNI", "MUL" or empty.
    pub key: String,
    pub nullable: bool,
    pub fk: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SchemaTable {
    pub name: String,
    pub view: bool,
    /// The server's estimate; None for views.
    pub rows: Option<u64>,
    pub cols: Vec<SchemaCol>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    pub tables: Vec<SchemaTable>,
}

impl Schema {
    /// Case-insensitive, like the server on the platforms where table names fold.
    pub fn table(&self, name: &str) -> Option<&SchemaTable> {
        self.tables.iter().find(|t| t.name == name).or_else(|| self.tables.iter().find(|t| t.name.eq_ignore_ascii_case(name)))
    }
}

impl SchemaTable {
    // Read by the tests that check what the schema query found.
    #[allow(dead_code)]
    pub fn col(&self, name: &str) -> Option<&SchemaCol> {
        self.cols.iter().find(|c| c.name == name).or_else(|| self.cols.iter().find(|c| c.name.eq_ignore_ascii_case(name)))
    }
}

/// The declared members of an `enum('a','b')` or `set(...)` type, with `''` read as one quote. Empty
/// for any other type.
pub fn enum_values(ty: &str) -> Vec<String> {
    let l = ty.trim();
    let Some(body) = l.get(..4).filter(|p| p.eq_ignore_ascii_case("enum") || p.eq_ignore_ascii_case("set(")).map(|_| l) else { return Vec::new() };
    let Some(open) = body.find('(') else { return Vec::new() };
    let (mut out, mut cur, mut inside) = (Vec::new(), String::new(), false);
    let mut chars = body[open + 1..].chars().peekable();
    while let Some(c) = chars.next() {
        match (inside, c) {
            (false, '\'') => inside = true,
            (true, '\'') if chars.peek() == Some(&'\'') => {
                cur.push('\'');
                chars.next();
            }
            (true, '\'') => {
                inside = false;
                out.push(std::mem::take(&mut cur));
            }
            // A backslash escapes the next char in the declaration text the server returns.
            (true, '\\') => cur.extend(chars.next()),
            (true, c) => cur.push(c),
            (false, _) => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_members_are_read_with_quotes() {
        assert_eq!(enum_values("enum('paid','shipped','it''s')"), ["paid", "shipped", "it's"]);
        assert_eq!(enum_values("set('a','b')"), ["a", "b"]);
        assert!(enum_values("varchar(20)").is_empty());
        assert!(enum_values("int").is_empty());
    }
}
