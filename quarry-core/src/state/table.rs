//! State behind src/app/table.rs, which draws it.

use crate::db;
use crate::state::cells::{DocRef, Val};
use mongodb::bson::{Bson, Document};

/// The rows a results table draws: result documents, or the Documents view's rows kept as BSON bytes.
#[derive(Clone, Copy)]
pub enum Rows<'a> {
    Docs(&'a [Document]),
    Raw(&'a [db::Row]),
}

impl<'a> Rows<'a> {
    pub fn len(self) -> usize {
        match self {
            Rows::Docs(d) => d.len(),
            Rows::Raw(r) => r.len(),
        }
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    pub fn slice(self, range: std::ops::Range<usize>) -> Rows<'a> {
        match self {
            Rows::Docs(d) => Rows::Docs(&d[range]),
            Rows::Raw(r) => Rows::Raw(&r[range]),
        }
    }

    /// Where the rows live, part of the column width cache key.
    pub fn addr(self) -> usize {
        match self {
            Rows::Docs(d) => d.as_ptr() as usize,
            Rows::Raw(r) => r.as_ptr() as usize,
        }
    }

    /// The value at a dotted path, read without decoding the rest of the row.
    pub fn value(self, i: usize, path: &str) -> Option<Val<'a>> {
        match self {
            Rows::Docs(d) => db::get_path(d.get(i)?, path).map(Val::B),
            Rows::Raw(r) => r.get(i)?.value(path).map(Val::R),
        }
    }

    /// Row `i` decoded; a raw row keeps it until the row closes, so only call it for open rows.
    pub fn doc(self, i: usize) -> Option<&'a Document> {
        match self {
            Rows::Docs(d) => d.get(i),
            Rows::Raw(r) => r.get(i).map(db::Row::doc),
        }
    }

    /// The top-level value `key` of row `i`, for rows whose keys may hold a dot.
    pub fn key(self, i: usize, key: &str) -> Option<Bson> {
        match self {
            Rows::Docs(d) => d.get(i)?.get(key).cloned(),
            Rows::Raw(r) => Bson::try_from(r.get(i)?.raw().get(key).ok()??).ok(),
        }
    }

    /// A decoded copy of row `i`, for actions.
    pub fn owned(self, i: usize) -> Option<Document> {
        match self {
            Rows::Docs(d) => d.get(i).cloned(),
            Rows::Raw(r) => r.get(i).map(db::Row::owned),
        }
    }

    pub fn id(self, i: usize) -> Option<Bson> {
        match self {
            Rows::Docs(d) => d.get(i)?.get("_id").cloned(),
            Rows::Raw(r) => r.get(i)?.id(),
        }
    }

    pub fn doc_ref(self, i: usize) -> DocRef<'a> {
        match self {
            Rows::Docs(d) => DocRef::B(&d[i]),
            Rows::Raw(r) => DocRef::from(&r[i]),
        }
    }
}
