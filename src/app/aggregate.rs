//! The aggregation pipeline builder.

use super::*;
use crate::ui::dnd;
use quarry_core::action::aggregate::AggA;
use quarry_core::action::Action;

pub(super) use quarry_core::action::aggregate::StageA as StageAct;

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
                if n > 1 {
                    ui.label(RichText::new("Drag or Alt+Up/Down to reorder").size(TXT_SMALL).color(FAINT()));
                }
            });
        });
        ui.horizontal_top(|ui| {
            let w = (ui.available_width() * 0.45).max(260.0);
            ui.allocate_ui_with_layout(egui::vec2(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(w);
                let id = egui::Id::unique(("stage-editor", s.uid));
                let rows = s.text.lines().count().clamp(2, 16);
                // A formatted stage goes back into the pipeline text, as an edit does.
                if crate::ui::editor::format_on_key(ui, id, &mut s.text) {
                    acts.push(StageAct::Edited(i));
                }
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

/// A stage card while it is dragged: its number, operator and the first lines of its body, painted without the editors
/// and menus that would otherwise follow the pointer.
fn lifted_stage(ui: &mut egui::Ui, i: usize, s: &Stage) -> egui::Response {
    let rect = ui.max_rect().shrink2(egui::vec2(12.0, 6.0));
    let p = ui.painter().with_clip_rect(rect);
    let head = p.layout_job(LayoutJob::single_section(format!("{}  {}", i + 1, s.op), mono_fmt(TXT_BODY, if s.on { C_KEYWORD() } else { FAINT() }, W_SEMIBOLD)));
    let y = rect.top() + 8.0;
    p.galley(egui::pos2(rect.left(), y), head.clone(), TEXT());
    let body: String = s.text.lines().take(6).collect::<Vec<_>>().join("\n");
    let g = p.layout(body, FontId::monospace(TXT_SMALL), MUTED(), rect.width());
    p.galley(egui::pos2(rect.left(), y + head.size().y + 8.0), g, MUTED());
    ui.response()
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
        // The page draws before the windows over it, so a Cmd+Enter meant for the palette or a dialog reaches it first.
        let covered = self.window_open();
        let mut run = false;
        let mut explain = false;
        let mut text_id = None;
        let pick = self.pick_data(ui.ctx(), PickKind::Pipeline);
        let mut pick_acts = Vec::new();
        let mut demo_pick = None;
        let mut code_lang = None;
        let mut stage = None;
        let mut sacts: Vec<StageAct> = Vec::new();
        self.core.sync_stages();
        let auto = self.core.prefs.auto_preview;
        let mut auto_now = auto;
        let t = self.tm().unwrap();
        let (tab, builder) = (t.uid, !t.agg_text_mode);
        let mut text_mode = t.agg_text_mode;
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
                segmented(ui, &mut text_mode, &[(false, "Stages"), (true, "Text")]);
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
                            // Cards drag to reorder with the whole card as the handle; the editor and the sample list
                            // keep their own drags (select text, scroll). The engine needs each card's height before it
                            // draws, so it uses the height the card had on the last frame and redraws when it changed.
                            let hid = egui::Id::unique("stage-heights");
                            let mut heights: HashMap<u64, f32> = ui.ctx().data(|d| d.get_temp(hid)).unwrap_or_default();
                            let sizes: Vec<f32> = t.stages.iter().map(|s| heights.get(&s.uid).copied().unwrap_or(160.0)).collect();
                            let keys: Vec<egui::Id> = t.stages.iter().map(|s| egui::Id::unique(("stage", s.uid))).collect();
                            let mut grew = false;
                            let mv = dnd::reorder(ui, egui::Id::unique("stage-cards"), dnd::Items::vertical(&sizes).keys(&keys).menu(), |ui, i, st| {
                                let s = &mut t.stages[i];
                                if st.lifted {
                                    return lifted_stage(ui, i, s);
                                }
                                let top = ui.max_rect().top();
                                // The header strip is the card's own focusable widget, under the operator menu and the
                                // switch, so Tab or a click on it gives the card the focus that Alt+Up and Alt+Down move,
                                // and a right click on it offers Move up and Move down.
                                let strip = egui::Rect::from_min_size(egui::pos2(ui.max_rect().left(), top + 6.0), egui::vec2(ui.max_rect().width(), 34.0));
                                let head = ui.interact(strip, ui.scope_id().with("stage-head"), Sense::click());
                                head.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, format!("Stage {} {}", i + 1, s.op)));
                                if head.clicked() {
                                    head.request_focus();
                                }
                                // A selectable label would take a drag that starts on it for selecting its text.
                                ui.style_mut().interaction.selectable_labels = false;
                                ui.add_space(6.0);
                                stage_card(ui, i, n, s, auto, &mut sacts);
                                let h = ui.min_rect().bottom() - top;
                                if heights.get(&s.uid).is_none_or(|old| (old - h).abs() > 0.5) {
                                    heights.insert(s.uid, h);
                                    grew = true;
                                }
                                head
                            });
                            if grew {
                                ui.ctx().request_repaint();
                            }
                            heights.retain(|uid, _| t.stages.iter().any(|s| s.uid == *uid));
                            ui.ctx().data_mut(|d| d.insert_temp(hid, heights));
                            if let Some(m) = mv {
                                sacts.push(StageAct::Move(m.from, m.to));
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
                text_id = Some(id);
                crate::ui::editor::format_on_key(ui, id, &mut t.agg_txt);
                crate::ui::vim::wrap(ui, id, &mut t.agg_txt, |ui, text| with_autopair(ui, id, text, |ui, text| egui::TextEdit::multiline(text).id(id).code_editor().desired_width(f32::INFINITY).desired_rows(8).layouter(&mut code_layout).show(ui)));
            }
            ui.horizontal(|ui| {
                let (main, tip) = run_split(ui, "Run", "⌘↵", true, "Choose what to run");
                run = main.on_hover_text("Run the whole pipeline (Cmd+Enter)").clicked() || (!covered && ui.input(|i| i.modifiers.command && i.key_pressed(Key::Enter)));
                // Explain and Copy as code are ways to run or read the pipeline, so they live with Run.
                egui::Popup::menu(&tip.on_hover_text("Run options")).show(|ui| {
                    menu_look(ui);
                    if ui.button("Explain").on_hover_text("Cmd+E").clicked() {
                        explain = true;
                        ui.close();
                    }
                    if let Some(id) = text_id
                        && ui.button("Format").on_hover_text("Indent the pipeline by its brackets and break long lines (Shift+Alt+F)").clicked()
                    {
                        crate::ui::editor::format_in(ui.ctx(), id, &mut t.agg_txt);
                        ui.close();
                    }
                    code_menu(ui, &mut code_lang);
                });
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
        if text_mode != !builder {
            self.exec(Action::Aggregate(AggA::TextMode { tab, on: text_mode }));
        }
        if auto_now != auto {
            self.exec(Action::Aggregate(AggA::AutoPreview(auto_now)));
        }
        if !sacts.is_empty() {
            self.exec(Action::Aggregate(AggA::Stages { tab, acts: sacts }));
        }
        if builder && unsynced.is_none() && let Some(at) = self.core.pump_previews() {
            ui.ctx().request_repaint_after(at.saturating_duration_since(std::time::Instant::now()));
        }
        if let Some(l) = code_lang {
            self.show_code(l, false);
        }
        if let Some(b) = demo_pick {
            self.demo_open_picker(ui.ctx(), &b, "picker-agg");
        }
        if let (Some(stage), false) = (stage, run) {
            self.exec(Action::Aggregate(AggA::RunTo { tab, stage }));
        }
        if run {
            self.exec(Action::Aggregate(AggA::Run { tab }));
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
}
