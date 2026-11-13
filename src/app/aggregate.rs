//! The aggregation pipeline builder.

use super::*;

pub(super) enum StageAct {
    Edited(usize),
    Toggled(usize),
    /// Move a stage one place, down when true.
    Move(usize, bool),
    Remove(usize),
    Add(String),
    Op(usize, String),
    /// Show 10 more documents; `usize::MAX` is the collection sample.
    More(usize),
    Preview(usize),
}

/// One stage of the pipeline builder: operator, on/off, order and removal on top, the stage body
/// on the left and what comes out of the stage on the right.
pub(super) fn stage_card(ui: &mut egui::Ui, i: usize, n: usize, s: &mut Stage, auto: bool, acts: &mut Vec<StageAct>) {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{}", i + 1)).monospace().color(FAINT()));
            ui.menu_button(semibold(RichText::new(&s.op).monospace()).color(if s.on { C_KEYWORD() } else { FAINT() }), |ui| {
                menu_look(ui);
                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                    for (op, _) in db::STAGES {
                        if ui.button(RichText::new(*op).monospace()).clicked() {
                            acts.push(StageAct::Op(i, op.to_string()));
                            ui.close();
                        }
                    }
                });
            })
            .response
            .on_hover_text("Change the stage operator");
            if ui.checkbox(&mut s.on, "").on_hover_text("Include this stage in the pipeline").changed() {
                acts.push(StageAct::Toggled(i));
            }
            ui.hyperlink_to(RichText::new("Docs").size(TXT_SMALL), format!("https://www.mongodb.com/docs/manual/reference/operator/aggregation/{}/", s.op.trim_start_matches('$')));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button(RichText::new("Remove…").color(ALERT()), |ui| {
                    menu_look(ui);
                    if danger(ui, format!("Remove stage {}", i + 1)).clicked() {
                        acts.push(StageAct::Remove(i));
                        ui.close();
                    }
                });
                if ui.add_enabled(i + 1 < n, egui::Button::new("⏷")).on_hover_text("Move this stage down").clicked() {
                    acts.push(StageAct::Move(i, true));
                }
                if ui.add_enabled(i > 0, egui::Button::new("⏶")).on_hover_text("Move this stage up").clicked() {
                    acts.push(StageAct::Move(i, false));
                }
            });
        });
        ui.horizontal_top(|ui| {
            let w = (ui.available_width() * 0.45).max(260.0);
            ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(w);
                let id = egui::Id::unique(("stage-editor", s.uid));
                let rows = s.text.lines().count().clamp(2, 16);
                let out = crate::ui::vim::wrap(ui, id, &mut s.text, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(rows).layouter(&mut code_layout).show(ui)));
                out.response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::MultilineTextInput, true, format!("Stage {} editor", i + 1)));
                if out.response.changed() {
                    acts.push(StageAct::Edited(i));
                }
                if let Err(e) = db::parse_value(&s.text) {
                    ui.label(RichText::new(e).size(TXT_SMALL).color(ALERT()));
                }
            });
            // A gap, not ui.separator(): in a row a separator takes all the height left and
            // stretches the card down the whole view.
            ui.add_space(14.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    if !s.on {
                        ui.label(RichText::new("Off: the pipeline skips this stage.").color(FAINT()));
                    } else if s.req != 0 {
                        spin(ui);
                        ui.label(RichText::new("Previewing…").color(MUTED()));
                    } else if let Some(ms) = s.ms {
                        match s.count {
                            Some(n) => ui.label(semibold(RichText::new(format!("{} documents", group(&n.to_string()))).color(BRIGHT()))),
                            None => ui.label(RichText::new("Too many documents to count before the timeout").color(MUTED())),
                        };
                        ui.label(RichText::new(format!("{ms} ms")).color(FAINT()));
                    } else if s.error.is_none() && !auto && ui.button("Preview").clicked() {
                        acts.push(StageAct::Preview(i));
                    }
                });
                if s.on {
                    if let Some(e) = &s.error {
                        ui.label(RichText::new(e).size(TXT_SMALL).color(if e.starts_with("This stage writes") { WARNING() } else { ALERT() }));
                    }
                    stage_samples(ui, s, i, acts);
                }
            });
        });
    });
}

/// The first documents out of a stage, one line each. The whole row toggles the document open
/// or closed, and several can be open at once.
pub(super) fn stage_samples(ui: &mut egui::Ui, s: &mut Stage, i: usize, acts: &mut Vec<StageAct>) {
    if s.sample.is_empty() {
        if s.count == Some(0) {
            ui.label(RichText::new("No documents come out of this stage.").color(FAINT()));
        }
        return;
    }
    let max_h = if s.open.is_empty() { 220.0 } else { 440.0 };
    egui::ScrollArea::vertical().id_salt(("stage-sample", s.uid)).max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
        for (k, d) in s.sample.iter().enumerate() {
            let open = s.open.contains(&k);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, R_SM, HOVER());
            }
            chevron(ui.painter(), egui::pos2(rect.left() + 8.0, rect.center().y), open, if open { PRIMARY() } else { FAINT() });
            let g = ui.fonts_mut(|f| f.layout_job(cell_job(&Bson::Document(d.clone()))));
            let clip = ui.painter().with_clip_rect(rect.shrink2(egui::vec2(2.0, 0.0)));
            clip.galley(egui::pos2(rect.left() + 20.0, rect.center().y - g.size().y / 2.0), g, TEXT());
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(if open { "Click to close" } else { "Click to see the whole document" }).clicked() && !s.open.remove(&k) {
                s.open.insert(k);
            }
            if open {
                let text = db::to_edit_json(d);
                let rows = text.lines().count().min(24);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    // A &str buffer is read-only: the text can be selected and copied, not edited.
                    ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY).desired_rows(rows).layouter(&mut code_layout));
                });
            }
        }
        if s.count.is_none_or(|n| n > s.sample.len() as u64) && s.sample.len() as i64 >= s.limit && ui.button(format!("Show {} more", db::STAGE_SAMPLE)).clicked() {
            acts.push(StageAct::More(i));
        }
    });
}

impl App {
    pub(super) fn aggregate(&mut self, ui: &mut egui::Ui) {
        let mut run = false;
        let mut explain = false;
        let pick = self.pick_data(PickKind::Pipeline);
        let mut pick_acts = Vec::new();
        let mut demo_pick = None;
        let mut code_lang = None;
        let mut stage = None;
        let mut sacts: Vec<StageAct> = Vec::new();
        self.sync_stages();
        let auto = self.prefs.auto_preview;
        let mut auto_now = auto;
        let t = self.tm().unwrap();
        let builder = !t.agg_text_mode;
        let labels: Vec<String> = if builder || db::is_shell(&t.agg_txt) {
            Vec::new()
        } else {
            db::parse_pipeline(&t.agg_txt).map(|p| p.iter().map(|s| s.keys().next().cloned().unwrap_or_default()).collect()).unwrap_or_default()
        };
        // Why the text cannot be shown as stages, if it cannot.
        let unsynced = (builder && t.stages_src != t.agg_txt).then(|| if db::is_shell(&t.agg_txt) { "this is a shell command".to_string() } else { stages_from_text(&t.agg_txt).err().unwrap_or_default() });
        let has_results = t.agg_ms.is_some() || t.agg_req != 0;
        let avail = ui.available_height();
        toolbar(ui, |ui| {
            ui.horizontal(|ui| {
                caption(ui, if builder { "PIPELINE  ·  each stage shows what comes out of it" } else { "PIPELINE  ·  an array of stages, or paste db.coll.aggregate([...])" });
                ui.add_space(8.0);
                segmented(ui, &mut t.agg_text_mode, &[(false, "Stages"), (true, "Text")]);
                if builder {
                    ui.checkbox(&mut auto_now, "Auto preview").on_hover_text("Preview each stage a second after you stop typing. Every preview runs two read-only aggregations on the server.");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    demo_pick = Some(picker_button(ui, &pick, false, &mut pick_acts));
                });
            });
            if builder {
                match &unsynced {
                    Some(e) => {
                        ui.label(RichText::new(format!("Cannot show this pipeline as stages: {e}. Fix or run it in Text.")).color(WARNING()));
                    }
                    None => {
                        let max_h = if has_results { (avail * 0.55).max(260.0) } else { (avail - 70.0).max(260.0) };
                        egui::ScrollArea::vertical().id_salt("stages").max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                            egui::Frame::new().fill(BG()).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                let n = t.agg_input.count.map_or(String::new(), |n| format!("  ·  about {} documents", group(&n.to_string())));
                                // Closed at first, like the original's collection sample (isOpen: false, renderer line 111386).
                                egui::CollapsingHeader::new(RichText::new(format!("Collection input{n}")).color(MUTED())).id_salt("agg-input").default_open(false).show(ui, |ui| {
                                    stage_samples(ui, &mut t.agg_input, usize::MAX, &mut sacts);
                                });
                            });
                            let n = t.stages.len();
                            for (i, s) in t.stages.iter_mut().enumerate() {
                                ui.add_space(6.0);
                                stage_card(ui, i, n, s, auto, &mut sacts);
                            }
                            ui.add_space(6.0);
                            ui.menu_button(semibold(RichText::new("+ Add stage").color(PRIMARY())), |ui| {
                                menu_look(ui);
                                egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                                    for (k, (op, _)) in db::STAGES.iter().enumerate() {
                                        // The first ten are the original's most used stages (topStages).
                                        if k == 10 {
                                            ui.separator();
                                        }
                                        if ui.button(RichText::new(*op).monospace()).clicked() {
                                            sacts.push(StageAct::Add(op.to_string()));
                                            ui.close();
                                        }
                                    }
                                });
                            });
                        });
                    }
                }
            } else {
                let id = ui.make_persistent_id("agg-editor");
                crate::ui::vim::wrap(ui, id, &mut t.agg_txt, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(8).layouter(&mut code_layout).show(ui)));
            }
            ui.horizontal(|ui| {
                run = run_button(ui, "Run", "⌘↵").on_hover_text("Run the whole pipeline (Cmd+Enter)").clicked() || ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter));
                explain = quiet(ui, "Explain").on_hover_text("Cmd+E").clicked();
                code_menu(ui, &mut code_lang);
                if t.agg_req != 0 {
                    spin(ui);
                    ui.label(RichText::new("Running…").color(MUTED()));
                } else if let Some(ms) = t.agg_ms {
                    let from = t.agg_stage.map_or(String::new(), |i| format!("Output after stage {}  ", i + 1));
                    ui.label(RichText::new(format!("{from}{} results  {} ms  (first 1000 shown)", t.agg_docs.len(), ms)).color(MUTED()));
                }
            });
            if !labels.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for (i, l) in labels.iter().enumerate() {
                        let fg = if t.agg_stage == Some(i) { PRIMARY() } else { MUTED() };
                        if chip(ui, &format!("{} {l}", i + 1), fg, true).on_hover_text("Preview the output after this stage").clicked() {
                            stage = Some(i);
                        }
                    }
                });
            }
        });
        if auto_now != auto {
            self.prefs.auto_preview = auto_now;
            self.save_prefs();
            if auto_now {
                self.stages_changed(0, false);
            }
        }
        self.apply_stage_acts(sacts);
        if builder && unsynced.is_none() {
            self.pump_previews(ui.ctx());
        }
        if let Some(l) = code_lang {
            self.show_code(l, false);
        }
        if let Some(b) = demo_pick {
            self.demo_open_picker(ui.ctx(), &b, "picker-agg");
        }
        let t = self.tm().unwrap();
        if let (Some(i), false) = (stage, run) && let Some(p) = db::parse_pipeline(&t.agg_txt).ok().and_then(|p| db::stage_preview(&p, i)) {
            self.run_pipeline(p);
            self.tm().unwrap().agg_stage = Some(i);
        }
        let t = self.tm().unwrap();
        if run {
            let text = t.agg_txt.clone();
            if db::is_shell(&text) {
                self.run_shell(&text);
            } else {
                match db::parse_pipeline(&text) {
                    Err(e) => self.err(e),
                    Ok(p) => self.run_pipeline(p),
                }
            }
        }
        if explain {
            self.explain();
        }
        ui.add_space(6.0);
        if !builder || has_results {
            self.results(ui, true);
        }
        // Last, since opening a script may change the tab this frame was drawn for.
        self.pick_acts(pick_acts);
    }

    /// Rebuilds the builder's stages when the pipeline text changed elsewhere (Text mode, the
    /// library, a pasted query).
    pub(super) fn sync_stages(&mut self) {
        let Some(t) = self.tm() else { return };
        if t.agg_text_mode || t.stages_src == t.agg_txt || db::is_shell(&t.agg_txt) {
            return;
        }
        if let Ok(st) = stages_from_text(&t.agg_txt) {
            t.stages = st;
            t.stages_src = t.agg_txt.clone();
        }
    }

    /// Stages from `from` on need a new preview (a second later while typing), and the pipeline
    /// text follows the stages.
    pub(super) fn stages_changed(&mut self, from: usize, typing: bool) {
        let auto = self.prefs.auto_preview;
        let t = self.tm().unwrap();
        let at = std::time::Instant::now() + std::time::Duration::from_millis(if typing { 1000 } else { 0 });
        for s in t.stages.iter_mut().skip(from) {
            s.due = auto.then_some(at);
        }
        t.agg_txt = text_from_stages(&t.stages);
        t.stages_src = t.agg_txt.clone();
    }

    pub(super) fn apply_stage_acts(&mut self, acts: Vec<StageAct>) {
        for a in acts {
            let t = self.tm().unwrap();
            match a {
                StageAct::Edited(i) => self.stages_changed(i, true),
                StageAct::Toggled(i) => self.stages_changed(i, false),
                StageAct::Move(i, down) => {
                    let j = if down { i + 1 } else { i.wrapping_sub(1) };
                    if j < t.stages.len() {
                        t.stages.swap(i, j);
                        self.stages_changed(i.min(j), false);
                    }
                }
                StageAct::Remove(i) => {
                    if i < t.stages.len() {
                        t.stages.remove(i);
                        self.stages_changed(i, false);
                    }
                }
                StageAct::Add(op) => {
                    let body = db::STAGES.iter().find(|(o, _)| *o == op).map_or("{}", |(_, b)| b);
                    t.stages.push(Stage::new(&op, body, true));
                    let i = t.stages.len() - 1;
                    self.stages_changed(i, false);
                }
                StageAct::Op(i, op) => {
                    let s = &mut t.stages[i];
                    // A body still equal to the old stage's example becomes the new stage's example.
                    let template = |o: &str| db::STAGES.iter().find(|(x, _)| *x == o).map(|(_, b)| *b);
                    if template(&s.op) == Some(s.text.as_str()) || s.text.trim().is_empty() {
                        s.text = template(&op).unwrap_or("{}").to_string();
                    }
                    s.op = op;
                    self.stages_changed(i, false);
                }
                StageAct::More(i) => {
                    let s = if i == usize::MAX { &mut t.agg_input } else { &mut t.stages[i] };
                    s.limit += db::STAGE_SAMPLE;
                    s.due = Some(std::time::Instant::now());
                }
                StageAct::Preview(i) => t.stages[i].due = Some(std::time::Instant::now()),
            }
        }
    }

    /// Runs the next stage preview that is due, one at a time and in order, like the original's
    /// getResultsForStages (renderer line 112079), so a pipeline never floods the server.
    pub(super) fn pump_previews(&mut self, ctx: &egui::Context) {
        if self.demo {
            return;
        }
        let Some(target) = self.target() else { return };
        let now = std::time::Instant::now();
        let t = &mut self.tabs[self.cur];
        if t.agg_input.req == 0 && t.agg_input.due.is_some_and(|d| d <= now) {
            t.agg_input.due = None;
            self.req += 1;
            t.agg_input.req = self.req;
            self.worker.stage_preview(self.req, target.clone(), Vec::new(), t.agg_input.limit);
        }
        if t.stages.iter().any(|s| s.req != 0) {
            return;
        }
        if let Some(next) = t.stages.iter().filter_map(|s| s.due).min() && next > now {
            ctx.request_repaint_after(next - now);
        }
        let Some(k) = t.stages.iter().position(|s| s.due.is_some_and(|d| d <= now)) else { return };
        t.stages[k].due = None;
        if !t.stages[k].on {
            return;
        }
        match stage_pipeline(&t.stages, k) {
            Err(e) => (t.stages[k].error, t.stages[k].count, t.stages[k].sample) = (Some(e), None, Vec::new()),
            // The original never previews past $out or $merge (lastStages, renderer line 112045).
            Ok(p) if db::has_write_stage(&p) => {
                (t.stages[k].error, t.stages[k].count, t.stages[k].sample) = (Some("This stage writes, so it only runs with the whole pipeline (Run).".into()), None, Vec::new());
            }
            Ok(p) => {
                self.req += 1;
                t.stages[k].req = self.req;
                self.worker.stage_preview(self.req, target, p, t.stages[k].limit);
            }
        }
    }
}
