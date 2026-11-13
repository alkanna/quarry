//! A read-only tree of a JSON value, folded like a Mongo document, for values viewed in full and SQL records.

use super::cells::{color_of, leaf_text};
use super::theme::*;
use super::widgets::*;
use crate::db;
use eframe::egui::{self, text::LayoutJob, FontId, RichText};
use mongodb::bson::Bson;
use std::sync::Arc;

/// Text that may be a JSON object or array, cheap enough to ask on every frame: it opens and closes with braces or
/// brackets, so a note such as "[draft] Ship Friday" is left alone.
pub(crate) fn looks_like(text: &str) -> bool {
    let t = text.trim();
    t.starts_with('{') && t.ends_with('}') || t.starts_with('[') && t.ends_with(']')
}

/// Members drawn per object or array; the rest are counted, since every open row is laid out on every frame.
const MAX_ROWS: usize = 500;

/// The text as a value to draw as a tree; JSON5 and shell syntax such as ObjectId() too. One answer is kept under `id`
/// for the text it was parsed from, since parsing a value of megabytes on every frame would stall the window.
pub(crate) fn parse(ctx: &egui::Context, id: egui::Id, text: &str) -> Result<Arc<Bson>, String> {
    let key = id.with("json-tree-parse");
    let hash = egui::util::hash(text);
    if let Some((h, v)) = ctx.data(|d| d.get_temp::<(u64, Result<Arc<Bson>, String>)>(key)) && h == hash {
        return v;
    }
    let v = match db::parse_value(text) {
        Ok(v @ (Bson::Document(_) | Bson::Array(_))) => Ok(Arc::new(v)),
        Ok(_) => Err("Not a JSON object or array.".to_string()),
        Err(e) => Err(e),
    };
    ctx.data_mut(|d| d.insert_temp(key, (hash, v.clone())));
    v
}

/// Drops the parsed value kept under `id`, once its tree is no longer shown.
pub(crate) fn forget(ctx: &egui::Context, id: egui::Id) {
    ctx.data_mut(|d| d.remove::<(u64, Result<Arc<Bson>, String>)>(id.with("json-tree-parse")));
}

/// The tree of `text` in a scroll area at most `max_h` tall, at least `min_h`.
pub(crate) fn scrolled(ui: &mut egui::Ui, id: egui::Id, text: &str, min_h: f32, max_h: f32) {
    egui::ScrollArea::both().id_salt(id.with("tree-scroll")).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        ui.set_min_height(min_h.min(max_h));
        show_text(ui, id, text);
    });
}

/// Whether the switch under `id` is on Tree, for text that looks like JSON.
pub(crate) fn is_tree(ui: &egui::Ui, id: egui::Id, text: &str) -> bool {
    looks_like(text) && ui.data(|d| d.get_temp(id.with("as-tree"))).unwrap_or(false)
}

/// The tree of `text` under its switch, or why it cannot be one.
pub(crate) fn show_text(ui: &mut egui::Ui, id: egui::Id, text: &str) {
    match parse(ui.ctx(), id, text) {
        Ok(v) => show(ui, id.with("tree"), &v),
        Err(e) => {
            ui.label(RichText::new(e).size(TXT_SMALL).color(ALERT()));
        }
    }
}

/// Text or tree, for a value that parses as JSON. Remembered per `id`, so the dialog opens the way it was left.
pub(crate) fn mode_switch(ui: &mut egui::Ui, id: egui::Id) -> bool {
    let key = id.with("as-tree");
    let mut tree = ui.data(|d| d.get_temp(key)).unwrap_or(false);
    segmented(ui, &mut tree, &[(false, "Text"), (true, "Tree")]);
    ui.data_mut(|d| d.insert_temp(key, tree));
    // Text mode needs no parsed copy kept alive.
    if !tree {
        forget(ui.ctx(), id);
    }
    tree
}

/// The tree with Expand all and Collapse all over it. The first two levels start open.
pub(crate) fn show(ui: &mut egui::Ui, id: egui::Id, v: &Bson) {
    let mut fold = None;
    ui.horizontal(|ui| {
        if quiet(ui, "Expand all").clicked() {
            fold = Some(true);
        }
        if quiet(ui, "Collapse all").clicked() {
            fold = Some(false);
        }
    });
    let mut path = Vec::new();
    children(ui, id, v, &mut path, fold);
}

fn children(ui: &mut egui::Ui, id: egui::Id, v: &Bson, path: &mut Vec<String>, fold: Option<bool>) {
    let (items, len): (Vec<(String, &Bson)>, usize) = match v {
        Bson::Document(d) => (d.iter().take(MAX_ROWS).map(|(k, v)| (k.clone(), v)).collect(), d.len()),
        Bson::Array(a) => (a.iter().take(MAX_ROWS).enumerate().map(|(i, v)| (i.to_string(), v)).collect(), a.len()),
        _ => return,
    };
    for (k, child) in items {
        path.push(k);
        node(ui, id, child, path, fold);
        path.pop();
    }
    if len > MAX_ROWS {
        ui.label(RichText::new(format!("{} more, in the text view", len - MAX_ROWS)).size(TXT_SMALL).color(FAINT()));
    }
}

fn node(ui: &mut egui::Ui, id: egui::Id, v: &Bson, path: &mut Vec<String>, fold: Option<bool>) {
    let key = path.last().cloned().unwrap_or_default();
    match v {
        Bson::Document(_) | Bson::Array(_) => {
            let mut job = LayoutJob::default();
            job.append(&key, 0.0, mono(BRIGHT()));
            job.append(&leaf_text(v), 10.0, mono(FAINT()));
            let r = egui::CollapsingHeader::new(job).id_salt(id.with(&*path)).default_open(path.len() < 2).open(fold).show(ui, |ui| children(ui, id, v, path, fold));
            menu(r.header_response, path, v);
        }
        leaf => {
            let r = ui
                .horizontal(|ui| {
                    ui.add_space(ui.spacing().indent);
                    ui.label(RichText::new(&key).font(FontId::monospace(MONO_PX)).color(MUTED()));
                    ui.add_space(6.0);
                    ui.add(egui::Label::new(RichText::new(leaf_text(leaf)).monospace().color(color_of(leaf))).wrap().sense(egui::Sense::click()))
                })
                .inner;
            menu(r, path, leaf);
        }
    }
}

fn menu(r: egui::Response, path: &[String], v: &Bson) {
    r.context_menu(|ui| {
        menu_look(ui);
        if ui.button("Copy value").clicked() {
            ui.ctx().copy_text(db::copy_text(v));
            ui.close();
        }
        if ui.button("Copy path").clicked() {
            ui.ctx().copy_text(path.join("."));
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_objects_and_arrays_become_trees() {
        let ctx = eframe::egui::Context::default();
        let id = eframe::egui::Id::unique("t");
        assert!(super::parse(&ctx, id, r#"{"a": [1, {"b": null}]}"#).is_ok());
        assert!(super::parse(&ctx, id, "[1, 2]").is_ok());
        assert!(super::parse(&ctx, id, "{ _id: ObjectId(\"65a1b2c3d4e5f60718293a4b\") }").is_ok());
        assert!(super::parse(&ctx, id, "{not json").is_err());
        assert!(super::parse(&ctx, id, "42").is_err());
        assert!(!super::looks_like("plain text") && super::looks_like("  [1] "));
        assert!(!super::looks_like("[draft] Ship Friday") && !super::looks_like("{not closed"));
    }
}
