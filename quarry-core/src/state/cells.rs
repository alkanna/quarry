//! State behind src/ui/cells.rs, which draws it.

use crate::db;
use mongodb::bson::{Bson, Document, RawBsonRef, RawDocument};
use std::borrow::Cow;

/// A value from a decoded document, or read straight from a row's BSON bytes (see `db::Row`).
#[derive(Clone, Copy)]
pub enum Val<'a> {
    B(&'a Bson),
    R(RawBsonRef<'a>),
}

/// A whole document, decoded or as BSON bytes.
#[derive(Clone, Copy)]
pub enum DocRef<'a> {
    B(&'a Document),
    R(&'a RawDocument),
}

impl<'a> From<&'a Document> for DocRef<'a> {
    fn from(d: &'a Document) -> Self {
        DocRef::B(d)
    }
}

impl<'a> From<&'a db::Row> for DocRef<'a> {
    fn from(r: &'a db::Row) -> Self {
        DocRef::R(r.raw())
    }
}

impl<'a> DocRef<'a> {
    pub fn fields(self) -> Box<dyn Iterator<Item = (&'a str, Val<'a>)> + 'a> {
        match self {
            DocRef::B(d) => Box::new(d.iter().map(|(k, v)| (k.as_str(), Val::B(v)))),
            DocRef::R(d) => Box::new(d.iter().map_while(Result::ok).map(|(k, v)| (k, Val::R(v)))),
        }
    }
}

impl<'a> Val<'a> {
    pub fn object(self) -> Option<DocRef<'a>> {
        match self {
            Val::B(Bson::Document(d)) => Some(DocRef::B(d)),
            Val::R(RawBsonRef::Document(d)) => Some(DocRef::R(d)),
            _ => None,
        }
    }

    pub fn items(self) -> Option<Box<dyn Iterator<Item = Val<'a>> + 'a>> {
        match self {
            Val::B(Bson::Array(a)) => Some(Box::new(a.iter().map(Val::B))),
            Val::R(RawBsonRef::Array(a)) => Some(Box::new(a.into_iter().map_while(Result::ok).map(Val::R))),
            _ => None,
        }
    }

    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Val::B(Bson::String(s)) => Some(s),
            Val::R(RawBsonRef::String(s)) => Some(s),
            _ => None,
        }
    }

    /// `db::type_name_kind`, without decoding a raw object, array or string.
    pub fn kind(self) -> &'static str {
        match self {
            Val::B(b) => db::type_name_kind(b),
            Val::R(RawBsonRef::Document(_)) => "object",
            Val::R(RawBsonRef::Array(_)) => "array",
            Val::R(RawBsonRef::String(_)) => "string",
            Val::R(_) => db::type_name_kind(&self.bson()),
        }
    }

    /// Checked without decoding a raw value, so it can run for every cell on screen.
    pub fn is_null(self) -> bool {
        matches!(self, Val::B(Bson::Null) | Val::R(RawBsonRef::Null))
    }

    /// The whole value of a cell that may be missing, which reads as NULL.
    pub fn owned(v: Option<Self>) -> Bson {
        v.map_or(Bson::Null, |v| v.bson().into_owned())
    }

    /// The whole value. A raw object or array gets decoded, so this is for one value at a time,
    /// not for every cell on every frame.
    pub fn bson(self) -> Cow<'a, Bson> {
        match self {
            Val::B(b) => Cow::Borrowed(b),
            Val::R(r) => Cow::Owned(Bson::try_from(r).unwrap_or(Bson::Null)),
        }
    }

    /// Whether it is an ObjectId or a string, the values a reference can be.
    pub fn id_like(self) -> bool {
        matches!(self, Val::B(Bson::ObjectId(_) | Bson::String(_)) | Val::R(RawBsonRef::ObjectId(_) | RawBsonRef::String(_)))
    }

    /// "{n fields}" or "[n items]" for an object or array.
    pub fn size_text(self) -> Option<String> {
        Some(match self {
            Val::B(Bson::Document(d)) => format!("{{{} fields}}", d.len()),
            Val::B(Bson::Array(a)) => format!("[{} items]", a.len()),
            Val::R(RawBsonRef::Document(d)) => format!("{{{} fields}}", d.iter().count()),
            Val::R(RawBsonRef::Array(a)) => format!("[{} items]", a.into_iter().count()),
            _ => return None,
        })
    }
}
