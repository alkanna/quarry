//! State behind src/app/whats_new.rs, which draws it.

use crate::update::Release;
use std::collections::HashSet;

pub struct WhatsNew {
    pub releases: Vec<Release>,
    /// The version Quarry was updated from, empty when the notes were opened by hand.
    pub from: String,
    /// The versions unfolded when several releases show. The newest starts open.
    pub open: HashSet<String>,
}

impl WhatsNew {
    pub fn new(releases: Vec<Release>, from: &str) -> Self {
        let open = releases.first().map(|r| r.version.clone()).into_iter().collect();
        Self { releases, from: from.to_string(), open }
    }

    pub fn title(&self) -> String {
        match self.releases.as_slice() {
            [one] => format!("What's new in Quarry {}", one.version),
            _ if !self.from.is_empty() => format!("What's new since Quarry {}", self.from),
            _ => "What's new in Quarry".into(),
        }
    }

    pub fn sub(&self) -> String {
        match self.releases.as_slice() {
            [one] => {
                let mut parts = Vec::new();
                if !one.date.is_empty() {
                    parts.push(format!("Released {}", nice_date(&one.date)));
                }
                if !self.from.is_empty() {
                    parts.push(format!("updated from {}", self.from));
                }
                parts.join("  ·  ")
            }
            many => match (many.first(), many.last()) {
                (Some(new), Some(old)) => format!("{} releases  ·  {} to {}", many.len(), old.version, new.version),
                _ => String::new(),
            },
        }
    }
}

/// "17 Nov 2026" from release-please's "2026-11-17".
pub fn nice_date(iso: &str) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let p: Vec<&str> = iso.split('-').collect();
    match (p.as_slice(), p.get(1).and_then(|m| m.parse::<usize>().ok())) {
        ([y, _, d], Some(m @ 1..=12)) => format!("{} {} {y}", d.trim_start_matches('0'), MONTHS[m - 1]),
        _ => iso.to_string(),
    }
}
