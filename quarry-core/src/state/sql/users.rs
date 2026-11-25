//! State behind src/app/sql/users.rs, which draws it.

use crate::db::sql::users::{self, Account, AccountInfo, AccountPlan, Edit, Expire, Grant, Loaded, Settings, Tls, UserList};
use crate::db;
use std::collections::BTreeSet;

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum UTab {
    Account,
    Global,
    Databases,
    Limits,
}

/// How the password lifetime is chosen; "After N days" has its number in a field beside it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ExpKind {
    Never,
    Default,
    Days,
}

/// One database of the Databases tab: what the server holds and what the boxes say now.
pub struct GrantRow {
    /// The database as the server's grant writes it, `\_` for a literal underscore.
    pub pattern: String,
    pub orig: BTreeSet<String>,
    pub now: BTreeSet<String>,
}

pub struct SqlUsersWin {
    pub conn: u64,
    pub list_req: u64,
    pub list: Option<db::Res<UserList>>,
    /// The label of each account of `list` in lowercase, for the filter box.
    pub search: Vec<String>,
    pub filter: String,
    /// (user, host) of the account on show.
    pub sel: Option<(String, String)>,
    pub tab: UTab,
    pub grants_req: u64,
    pub grants: Option<db::Res<Vec<Grant>>>,
    pub info: Option<AccountInfo>,
    /// Why the login settings are not shown.
    pub info_err: Option<String>,
    pub rows: Vec<GrantRow>,
    /// The name being typed for "+ Database".
    pub adding: Option<String>,
    /// An Apply is running.
    pub busy: bool,
    // Account tab.
    pub name: String,
    pub host: String,
    pub locked: bool,
    pub exp: ExpKind,
    pub days: String,
    pub tls: Tls,
    pub roles_orig: Vec<String>,
    pub roles_now: Vec<String>,
    pub role_add: Option<String>,
    // Global tab.
    pub g_orig: BTreeSet<String>,
    pub g_now: BTreeSet<String>,
    pub priv_add: Option<String>,
    /// Dynamic privileges held WITH GRANT OPTION, which this window does not revoke.
    pub dyn_grant: BTreeSet<String>,
    /// The server compares database names without case (lower_case_table_names is not 0).
    pub fold_case: bool,
    // Limits tab, as typed.
    pub limits: [String; 4],
}

impl SqlUsersWin {
    pub fn new(conn: u64) -> SqlUsersWin {
        SqlUsersWin {
            conn,
            list_req: 0,
            list: None,
            search: Vec::new(),
            filter: String::new(),
            sel: None,
            tab: UTab::Databases,
            grants_req: 0,
            grants: None,
            info: None,
            info_err: None,
            rows: Vec::new(),
            adding: None,
            busy: false,
            name: String::new(),
            host: String::new(),
            locked: false,
            exp: ExpKind::Default,
            days: "90".into(),
            tls: Tls::None,
            roles_orig: Vec::new(),
            roles_now: Vec::new(),
            role_add: None,
            g_orig: BTreeSet::new(),
            g_now: BTreeSet::new(),
            priv_add: None,
            dyn_grant: BTreeSet::new(),
            fold_case: false,
            limits: Default::default(),
        }
    }

    pub fn set_list(&mut self, res: db::Res<UserList>) {
        self.search = res.as_ref().map(|l| l.accounts.iter().map(|a| a.label().to_lowercase()).collect()).unwrap_or_default();
        self.list = Some(res);
    }

    pub fn edits(&self) -> Vec<Edit> {
        self.rows.iter().map(|r| users::diff(&r.pattern, &r.orig, &r.now)).filter(|e| !e.is_empty()).collect()
    }

    pub fn global_edits(&self) -> Vec<Edit> {
        Some(users::diff("", &self.g_orig, &self.g_now)).filter(|e| !e.is_empty()).into_iter().collect()
    }

    pub fn account(&self) -> Option<Account> {
        let (u, h) = self.sel.as_ref()?;
        let found = self.list.as_ref().and_then(|l| l.as_ref().ok()).and_then(|l| l.accounts.iter().find(|a| &a.user == u && &a.host == h));
        Some(found.cloned().unwrap_or_else(|| Account::named(u, h)))
    }

    /// The fields of the Account tab from the server's values, and the other tabs' too.
    pub fn fill(&mut self, acct: &Account, l: &Loaded) {
        self.rows = users::db_grants(&l.grants).into_iter().map(|(pattern, s)| GrantRow { pattern, orig: s.clone(), now: s }).collect();
        self.rows.sort_by_key(|r| users::shown_pattern(&r.pattern).to_lowercase());
        self.adding = None;
        self.role_add = None;
        self.priv_add = None;
        self.g_orig = users::global_privs(&l.grants);
        self.g_now = self.g_orig.clone();
        self.dyn_grant = users::dynamic_with_grant(&l.grants);
        self.fold_case = l.fold_case;
        self.roles_orig = users::roles_of(&l.grants);
        self.roles_now = self.roles_orig.clone();
        self.name = acct.user.clone();
        self.host = acct.host.clone();
        self.info = l.info.clone();
        self.info_err = l.info_err.clone();
        let s = l.info.as_ref().map(|i| i.settings.clone()).unwrap_or_default();
        self.locked = s.locked;
        (self.exp, self.days) = match s.expire {
            Expire::Never => (ExpKind::Never, "90".into()),
            Expire::Default => (ExpKind::Default, "90".into()),
            Expire::Days(n) => (ExpKind::Days, n.to_string()),
        };
        self.tls = s.tls;
        self.limits = s.limits.map(|n| n.to_string());
    }

    pub fn orig_settings(&self) -> Settings {
        self.info.as_ref().map(|i| i.settings.clone()).unwrap_or_default()
    }

    /// The Account tab's changes: a rename, the lock, lifetime and TLS, and roles.
    pub fn account_plan(&self, acct: &Account) -> Result<AccountPlan, String> {
        let orig = self.orig_settings();
        let expire = match self.exp {
            ExpKind::Never => Expire::Never,
            ExpKind::Default => Expire::Default,
            ExpKind::Days => Expire::Days(self.days.trim().parse().map_err(|_| "Days is a whole number.".to_string())?),
        };
        // The requirement's values stay only while the requirement is the one that was read.
        let spec = if self.tls == Tls::Specified { orig.spec.clone() } else { Default::default() };
        let now = Settings { locked: self.locked, expire, tls: self.tls, spec, limits: orig.limits };
        let rename = ((self.name.as_str(), self.host.as_str()) != (acct.user.as_str(), acct.host.as_str())).then(|| Account::named(&self.name, &self.host));
        let diff = |a: &[String], b: &[String]| a.iter().filter(|r| !b.contains(r)).cloned().collect::<Vec<_>>();
        Ok(AccountPlan {
            rename,
            settings: self.info.is_some().then_some((orig, now)),
            roles_grant: diff(&self.roles_now, &self.roles_orig),
            roles_revoke: diff(&self.roles_orig, &self.roles_now),
        })
    }

    /// The Limits tab's changes.
    pub fn limits_plan(&self) -> Result<AccountPlan, String> {
        let orig = self.orig_settings();
        let mut now = orig.clone();
        for (i, t) in self.limits.iter().enumerate() {
            now.limits[i] = t.trim().parse().map_err(|_| "A limit is a whole number, 0 for none.".to_string())?;
        }
        Ok(AccountPlan { settings: self.info.is_some().then_some((orig, now)), ..Default::default() })
    }

    /// Whether a tab holds staged changes, for the mark on its title.
    pub fn staged(&self, tab: UTab, acct: &Account) -> bool {
        match tab {
            UTab::Databases => !self.edits().is_empty(),
            UTab::Global => !self.global_edits().is_empty(),
            UTab::Account => self.account_plan(acct).map_or(true, |p| !p.is_empty()),
            UTab::Limits => self.limits_plan().map_or(true, |p| !p.is_empty()),
        }
    }
}

pub struct NewUser {
    pub conn: u64,
    pub user: String,
    pub host: String,
    /// Typed here and handed to the worker by Create; never shown, logged or kept after.
    pub password: String,
    /// Makes the password field's id new for each dialog, so the text editor's undo history of an earlier one is out of reach.
    pub serial: u64,
}

pub struct PwForm {
    pub conn: u64,
    pub acct: Account,
    pub plugin: String,
    /// The plugin the account has now, for the plugins on offer.
    pub current: String,
    /// Why the password cannot be changed from here (MariaDB accounts with another way in).
    pub blocked: Option<String>,
    pub password: String,
    pub again: String,
    pub serial: u64,
}
