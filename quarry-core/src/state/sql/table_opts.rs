//! State behind src/app/sql/table_opts.rs, which draws it.

use crate::db::sql::create::Meta;
use crate::db::sql::ddl::Change;

/// The Edit table options dialog.
pub struct TableForm {
    pub conn: u64,
    /// The database, which the lists of engines and collations are read for.
    pub schema: String,
    pub pg: bool,
    /// What the structure read.
    pub was: (String, String, String, Option<u64>),
    pub comment: String,
    pub engine: String,
    pub collation: String,
    pub auto: String,
    /// The staged changes that are not table options, which these go after.
    pub rest: Vec<Change>,
    /// Whether table options were already staged when the dialog opened, so clearing the fields back can still be staged.
    pub had_staged: bool,
    pub meta: Option<Meta>,
    pub load_err: Option<String>,
    pub view: Option<TableView>,
}

/// The plan of the options as they stand, kept until a field changes.
pub struct TableView {
    pub key: [String; 4],
    /// The form did not parse, which is a mistake to point at and not a form left alone.
    pub invalid: bool,
    /// Nothing differs from the server any more, but options were staged before: staging then takes them back.
    pub removes: bool,
    /// Only the table options of the form. They replace the staged ones, so opening the dialog twice never stacks two.
    pub new: Vec<Change>,
    pub text: Result<String, String>,
    pub warnings: Vec<String>,
}
