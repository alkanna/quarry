//! The connection form: engine, address, auth, SSH, TLS and proxy settings, with Test and Save.

use super::*;

/// One engine in the left column. Disabled cards show what comes later.
fn engine_card(ui: &mut egui::Ui, code: &str, name: &str, sub: &str, color: Color32, selected: bool, enabled: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 50.0), if enabled { Sense::click() } else { Sense::hover() });
    let state = if enabled { state_of(&r) } else { 0 };
    let p = ui.painter();
    let fill = match (selected, state) {
        (true, 0) => SEL(),
        (true, _) => SEL_HOVER(),
        (false, s) => hover_fill(s),
    };
    p.rect(rect, R_MD, PANEL(), Stroke::new(1.0, if selected { PRIMARY() } else { BORDER() }), egui::StrokeKind::Inside);
    p.rect_filled(rect.shrink(1.0), R_MD, fill);
    let fg = if enabled { color } else { FAINT() };
    let mark = egui::Rect::from_min_size(rect.left_center() + egui::vec2(12.0, -14.0), egui::vec2(28.0, 28.0));
    p.rect_filled(mark, R_SM, tint(fg, 40));
    let g = p.layout_job(egui::text::LayoutJob::single_section(code.to_string(), mono_fmt(TXT_SMALL, fg, W_SEMIBOLD)));
    p.galley(mark.center() - g.size() / 2.0 + egui::vec2(0.0, optical_dy(&g)), g, fg);
    let title = p.layout_job(egui::text::LayoutJob::single_section(name.to_string(), ui_fmt(TXT_BODY, Color32::PLACEHOLDER, W_SEMIBOLD, 0.0)));
    let sub = p.layout_job(egui::text::LayoutJob::single_section(sub.to_string(), ui_fmt(TXT_SMALL, Color32::PLACEHOLDER, 400.0, 0.0)));
    let x = mark.right() + 10.0;
    p.galley(egui::pos2(x, rect.center().y - 16.0), title, if enabled { BRIGHT() } else { FAINT() });
    p.galley(egui::pos2(x, rect.center().y + 1.0), sub, if enabled { MUTED() } else { FAINT() });
    focus_ring(ui, &r, rect, R_MD as f32);
    let label = name.to_string();
    r.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, label.clone()));
    if enabled { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r }
}

/// The tabs of the connection form: Mongo reaches its servers through a proxy, SQL engines through TLS settings and, where
/// the engine has them, session settings.
fn conn_tabs(engine: db::sql::Engine) -> &'static [(ConnTab, &'static str)] {
    if engine.is_sql() { &[(ConnTab::Standard, "Standard"), (ConnTab::Ssh, "SSH"), (ConnTab::Tls, "TLS"), (ConnTab::Advanced, "Advanced")] } else { &[(ConnTab::Standard, "Standard"), (ConnTab::Ssh, "SSH"), (ConnTab::Proxy, "Proxy")] }
}

fn tls_tab(ui: &mut egui::Ui, f: &mut ConnForm) {
    if f.cloud_on().is_some() {
        note_row(ui, C_LINK(), true, "The Cloud SQL proxy encrypts the connection with the instance's own certificate, so these settings are not used.");
        ui.add_space(8.0);
        ui.add_enabled_ui(false, |ui| tls_settings(ui, f));
    } else {
        tls_settings(ui, f);
    }
}

fn tls_settings(ui: &mut egui::Ui, f: &mut ConnForm) {
    settings_section(ui, "TLS", |ui| {
        egui::Grid::new("tls").num_columns(2).spacing([FORM_GAP, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            let pg = f.engine == db::sql::Engine::Postgres;
            form_label(ui, if pg { "Mode" } else { "SSL mode" });
            segmented(ui, &mut f.addr.ssl_mode, if pg { &db::sql::SslMode::PG_ALL } else { &db::sql::SslMode::ALL });
            ui.end_row();
            let file_row = |ui: &mut egui::Ui, label: &str, path: &mut String| {
                form_label(ui, label);
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(path).hint_text("optional, PEM file").desired_width(280.0));
                    if ui.button("Choose…").clicked() && let Some(p) = rfd::FileDialog::new().set_directory(dirs::home_dir().unwrap_or_default()).pick_file() {
                        *path = p.display().to_string();
                    }
                });
                ui.end_row();
            };
            file_row(ui, "CA file", &mut f.addr.ssl_ca);
            if pg {
                file_row(ui, "Client cert", &mut f.addr.ssl_cert);
                file_row(ui, "Client key", &mut f.addr.ssl_key);
                form_label(ui, "Passphrase");
                ui.add(egui::TextEdit::singleline(&mut f.pg_key_pass).password(true).hint_text("only for an encrypted key").desired_width(280.0));
                ui.end_row();
            }
        });
        ui.add_space(6.0);
        if f.engine == db::sql::Engine::Postgres {
            ui.add(egui::Label::new(RichText::new("Names follow libpq sslmode. prefer tries TLS and falls back to plain, require encrypts without checking the certificate, verify-ca checks the chain against the CA file and needs one, verify-full also checks the host name. With a CA file only that CA is trusted; verify-full without one trusts the public CAs. Through an SSH tunnel the name checked is still the host typed above.").size(TXT_SMALL).color(FAINT())).wrap());
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new("Certificate and key are PEM files. The passphrase is only for an encrypted key.").size(TXT_SMALL).color(FAINT())).wrap());
        } else {
            ui.add(egui::Label::new(RichText::new("Verify CA also checks the host name, a limit of Quarry's MySQL driver, and the public CAs are trusted beside the CA file. Through an SSH tunnel the name checked is the host typed above, except on Windows, where it is the tunnel's 127.0.0.1.").size(TXT_SMALL).color(FAINT())).wrap());
        }
    });
}

/// A timeout box: seconds, empty for the default, with the unit inside the box's right edge.
pub(super) fn secs_field(ui: &mut egui::Ui, text: &mut String, hint: &str) {
    ui.add(egui::TextEdit::singleline(text).hint_text(hint).desired_width(58.0).margin(FIELD_MARGIN));
    ui.label(RichText::new("s").size(TXT_SMALL).color(FAINT()));
}

fn advanced_tab(ui: &mut egui::Ui, f: &mut ConnForm, default_s: u64) {
    use db::sql::advanced::MODE_FLAGS;
    let note = |ui: &mut egui::Ui, t: &str| {
        ui.add(egui::Label::new(RichText::new(t).size(TXT_SMALL).color(FAINT())).wrap());
    };
    settings_section(ui, "SESSION", |ui| {
        let field_w = ui.available_width() - FORM_LABEL_W - FORM_GAP;
        egui::Grid::new("adv-session").num_columns(2).spacing([FORM_GAP, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Time zone");
            ui.horizontal(|ui| {
                segmented(ui, &mut f.zone_kind, &[(ZoneKind::Server, "Server default"), (ZoneKind::Utc, "UTC"), (ZoneKind::System, "System"), (ZoneKind::Custom, "Custom")]);
                if f.zone_kind == ZoneKind::Custom {
                    ui.add(egui::TextEdit::singleline(&mut f.zone_text).hint_text("+02:00 or Europe/Paris").desired_width(ui.available_width()).margin(FIELD_MARGIN));
                }
            });
            ui.end_row();
            ui.label("");
            ui.vertical(|ui| {
                ui.set_max_width(field_w);
                note(ui, "System sends this computer's offset. Custom takes an offset like +02:00, or a zone name like Europe/Paris when the server has its zone tables loaded.")
            });
            ui.end_row();
            form_label(ui, "Charset");
            ui.horizontal(|ui| {
                let before = f.adv.charset;
                egui::ComboBox::from_id_salt("adv-charset").width(110.0).selected_text(RichText::new(f.adv.charset.as_str()).monospace()).show_ui(ui, |ui| {
                    menu_look(ui);
                    for (c, name) in db::sql::Charset::ALL {
                        ui.selectable_value(&mut f.adv.charset, c, name);
                    }
                });
                // A collation belongs to its charset, so it is not kept across a change.
                if f.adv.charset != before {
                    f.adv.collation.clear();
                }
                ui.label("Collation");
                let cs = f.adv.charset.as_str();
                let default = format!("Default for {cs}");
                let known: Vec<&str> = f.collations.iter().filter(|(c, _)| c == cs).map(|(_, n)| n.as_str()).collect();
                if known.is_empty() {
                    ui.add(egui::TextEdit::singleline(&mut f.adv.collation).hint_text(default).font(egui::TextStyle::Monospace).desired_width(ui.available_width()).margin(FIELD_MARGIN));
                } else {
                    egui::ComboBox::from_id_salt("adv-collation").width(ui.available_width()).selected_text(if f.adv.collation.is_empty() { default.clone() } else { f.adv.collation.clone() }).show_ui(ui, |ui| {
                        menu_look(ui);
                        ui.selectable_value(&mut f.adv.collation, String::new(), default);
                        for n in known {
                            ui.selectable_value(&mut f.adv.collation, n.to_string(), n);
                        }
                    });
                }
            });
            ui.end_row();
            ui.label("");
            ui.vertical(|ui| {
                ui.set_max_width(field_w);
                note(ui, "UTF-8 charsets only, since Quarry decodes every result as UTF-8. Test reads the server's collations.");
                if f.adv.charset == db::sql::Charset::Utf8mb3 {
                    note(ui, "With utf8mb3, characters outside the BMP (such as emoji) come back as '?', and rows holding them cannot be edited in the grid.");
                }
            });
            ui.end_row();
        });
    });
    settings_section(ui, "SQL MODE", |ui| {
        egui::Grid::new("adv-mode").num_columns(2).spacing([FORM_GAP, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "SQL mode");
            ui.horizontal(|ui| {
                let mut custom = f.mode_custom as u8;
                segmented(ui, &mut custom, &[(0, "Server default"), (1, "Custom")]);
                f.mode_custom = custom == 1;
                f.seed_mode();
                if f.mode_custom {
                    ui.label(RichText::new("Starts from the server's mode on Test").size(TXT_SMALL).color(FAINT()));
                }
            });
            ui.end_row();
            if f.mode_custom {
                ui.label("");
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        for (flag, on) in MODE_FLAGS.iter().zip(f.mode_on.iter_mut()) {
                            if chip(ui, flag, PRIMARY(), *on).clicked() {
                                *on = !*on;
                                f.mode_seeded = true;
                            }
                        }
                    });
                    ui.add_space(4.0);
                    if ui.add(egui::TextEdit::singleline(&mut f.mode_extra).hint_text("More flags, comma separated").desired_width(ui.available_width()).margin(FIELD_MARGIN)).changed() {
                        f.mode_seeded = true;
                    }
                });
                ui.end_row();
            }
            ui.label("");
            note(ui, "Either way Quarry reads the effective sql_mode and quotes by it.");
            ui.end_row();
        });
    });
    settings_section(ui, "INIT COMMAND", |ui| {
        let mut layout = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap: f32| {
            let mut job = sql_job(buf.as_str(), TXT_SMALL);
            job.wrap.max_width = wrap;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        egui::Frame::new().fill(BG()).stroke(Stroke::new(1.0, BORDER())).corner_radius(R_MD).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut f.adv.init).font(FontId::monospace(TXT_SMALL)).frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(10, 8))).desired_rows(3).desired_width(f32::INFINITY).lock_focus(true).hint_text(RichText::new("SET SESSION group_concat_max_len = 1048576;").font(FontId::monospace(TXT_SMALL)).color(FAINT())).layouter(&mut layout));
        });
        ui.add_space(4.0);
        note(ui, "Runs on every pooled connection. Writes are refused while the connection is read-only.");
    });
    settings_section(ui, "TIMEOUTS", |ui| {
        let d = default_s.to_string();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Connect").color(MUTED()));
            secs_field(ui, &mut f.t_connect, &d);
            ui.add_space(6.0);
            ui.label(RichText::new("Query").color(MUTED()));
            secs_field(ui, &mut f.t_query, &d);
            ui.add_space(6.0);
            ui.label(RichText::new("Idle").color(MUTED()));
            secs_field(ui, &mut f.t_idle, "600");
        });
        ui.add_space(4.0);
        note(ui, "Empty uses the timeout in Settings. Idle closes pooled connections unused that long. At the query timeout MySQL stops only SELECT, MariaDB every statement.");
    });
}

/// What the paste field last did, kept in egui's memory because the form struct belongs to another file. The
/// fields a paste filled get a soft blue border until the user edits them. It is a tint of the accent, so it stays apart
/// from the solid accent border a field with focus has.
#[derive(Clone, Default)]
struct Paste {
    text: String,
    note: Option<Result<String, String>>,
    /// Host, port, user, password, database, TLS.
    filled: [bool; 6],
}

const PASTE_HOST: usize = 0;
const PASTE_PORT: usize = 1;
const PASTE_USER: usize = 2;
const PASTE_PASSWORD: usize = 3;
const PASTE_DATABASE: usize = 4;
const PASTE_TLS: usize = 5;

fn paste_id(f: &ConnForm) -> egui::Id {
    egui::Id::NULL.with(("pg-paste", f.id))
}

/// Forgets the paste field, so the next form starts clean.
fn paste_reset(ctx: &egui::Context, f: &ConnForm) {
    ctx.data_mut(|d| d.remove::<Paste>(paste_id(f)));
}

/// Marks a field the paste filled, and unmarks it once the user types in it.
fn filled_border(ui: &egui::Ui, r: &egui::Response, mark: &mut bool) {
    if r.changed() {
        *mark = false;
    }
    if *mark {
        ui.painter().rect_stroke(r.rect, R_SM, Stroke::new(1.0, tint(PRIMARY(), 120)), egui::StrokeKind::Inside);
    }
}

/// Fills the form from the pasted text and records what it did. Shared with the offline demo, which seeds the same state.
fn paste_apply(st: &mut Paste, f: &mut ConnForm) {
    match db::sql::Addr::from_paste(&st.text) {
        Ok(a) => {
            let had_password = !a.password.is_empty();
            let cert = !a.ssl_cert.is_empty() || !a.ssl_key.is_empty();
            st.filled = [true, true, !a.user.is_empty(), had_password, !a.database.is_empty(), a.ssl_mode != db::sql::SslMode::Preferred || !a.ssl_ca.is_empty() || cert];
            let mut parts = vec!["host", "port"];
            for (i, w) in [(PASTE_USER, "user"), (PASTE_PASSWORD, "password"), (PASTE_DATABASE, "database")] {
                if st.filled[i] {
                    parts.push(w);
                }
            }
            let mut note = format!("Filled {}", parts.join(", "));
            if st.filled[PASTE_TLS] {
                note += &format!(" and TLS {}", a.ssl_mode.pg_name());
                if cert {
                    note += " with a client certificate";
                }
            }
            note += if had_password { "." } else { ". No password in the URL, type it below." };
            f.addr = db::sql::Addr { ssl_mode: a.ssl_mode, ..a };
            // A password in the box would stay readable on screen, so it moves to the masked field.
            if had_password {
                st.text.clear();
            }
            st.note = Some(Ok(note));
        }
        Err(e) => {
            st.filled = [false; 6];
            st.note = Some(Err(e));
        }
    }
}

/// QUARRY_TEST_PG=paste: what the design shows after a paste. The form struct has no egui context at load time, so the
/// first frame of a fresh paste field seeds it. The design types the password after the paste, so it is set here too.
fn paste_seed(ctx: &egui::Context, f: &mut ConnForm) {
    let id = paste_id(f);
    if std::env::var("QUARRY_TEST_PG").as_deref() != Ok("paste") || ctx.data(|d| d.get_temp::<Paste>(id)).is_some_and(|st| st.note.is_some()) {
        return;
    }
    let mut st = Paste { text: "postgresql://analyst@analytics-prod.internal:5432/analytics?sslmode=verify-full".into(), ..Default::default() };
    paste_apply(&mut st, f);
    f.addr.password = "hunter2hunter".into();
    st.filled[PASTE_PASSWORD] = true;
    ctx.data_mut(|d| d.insert_temp(id, st));
}

/// A line of small text led by a dot: a plain one for a result, or with an "i" for a notice.
pub(super) fn note_row(ui: &mut egui::Ui, dot: Color32, info: bool, text: &str) {
    ui.horizontal_top(|ui| {
        if info {
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), INFO_DOT_R, tint(dot, 60));
            let g = ui.painter().layout_job(egui::text::LayoutJob::single_section("i".into(), ui_fmt(TXT_SMALL, dot, W_SEMIBOLD, 0.0)));
            ui.painter().galley(r.center() - g.size() / 2.0 + egui::vec2(0.0, optical_dy(&g)), g, dot);
        } else {
            let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 14.0), Sense::hover());
            ui.painter().circle_filled(r.center(), STATUS_DOT_R, dot);
        }
        ui.add(egui::Label::new(RichText::new(text).size(TXT_SMALL).color(MUTED())).wrap());
    });
}

fn paste_field(ui: &mut egui::Ui, f: &mut ConnForm, field_w: f32) -> Paste {
    let id = paste_id(f);
    let mut st: Paste = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal(|ui| {
        caption(ui, "PASTE CONNECTION STRING");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("Fills the fields below").size(TXT_SMALL).color(FAINT()));
        });
    });
    let r = ui.add(egui::TextEdit::singleline(&mut st.text).font(egui::TextStyle::Monospace).hint_text("postgresql://user@host:5432/database?sslmode=verify-full").desired_width(field_w + FORM_LABEL_W + FORM_GAP).margin(FIELD_MARGIN_TALL));
    if r.changed() && !st.text.trim().is_empty() {
        paste_apply(&mut st, f);
    } else if r.changed() {
        st.note = None;
    }
    match &st.note {
        Some(Ok(m)) => note_row(ui, SUCCESS(), false, m),
        Some(Err(e)) => {
            ui.add(egui::Label::new(RichText::new(e).size(TXT_SMALL).color(ALERT())).wrap());
        }
        None => {}
    }
    ui.data_mut(|d| d.insert_temp(id, st.clone()));
    st
}

/// The rows that replace host, port and password while the connection goes through Cloud SQL. True when Change is clicked.
fn cloud_rows(ui: &mut egui::Ui, f: &mut ConnForm, cloud: &super::cloud_sql::CloudCache, field_w: f32) -> bool {
    let Some(mut conf) = f.cloud_sql.clone() else { return false };
    let inst = f.cloud_inst.clone().or_else(|| cloud.instance(&conf.instance).cloned());
    let account = cloud.account();
    let mut change = false;
    form_label(ui, "Cloud SQL");
    let (rect, _) = ui.allocate_exact_size(egui::vec2(field_w, BUTTON_H + 1.0), Sense::hover());
    ui.painter().rect(rect, R_MD, PANEL(), Stroke::new(1.0, BORDER()), egui::StrokeKind::Inside);
    // The links go first, so a long instance name is cut before it runs under them.
    let mut host = false;
    let links = put_at(ui, rect.shrink2(egui::vec2(10.0, 0.0)), |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            let h = ui.link(RichText::new("Use a host").size(TXT_SMALL));
            let c = ui.link(RichText::new("Change").size(TXT_SMALL));
            host = h.clicked();
            change = c.clicked();
            c.rect.left()
        })
        .inner
    });
    let p = ui.painter().with_clip_rect(egui::Rect::from_min_max(rect.min, egui::pos2(links - 10.0, rect.bottom())));
    let dot = egui::pos2(rect.left() + 12.0, rect.center().y);
    match &inst {
        Some(i) if i.state != "RUNNABLE" => p.circle_stroke(dot, 3.0, Stroke::new(1.0, FAINT())),
        Some(_) => p.circle_filled(dot, 3.5, SUCCESS()),
        None => p.circle_filled(dot, 3.5, FAINT()),
    };
    let g = p.layout_job(egui::text::LayoutJob::single_section(conf.instance.clone(), mono_fmt(TXT_BODY, TEXT(), 400.0)));
    let x = rect.left() + 22.0 + g.size().x + 8.0;
    galley_at(&p, rect.left() + 22.0, rect.center().y, g, TEXT());
    // A badge cut in half reads worse than none; IAM sign-in is also shown by its switch below.
    if conf.iam && x + 40.0 <= links - 10.0 {
        super::cloud_sql::iam_badge(&p, x, rect.center().y);
    }
    ui.end_row();
    form_label(ui, "IP address");
    ui.horizontal(|ui| {
        let (opts, note) = super::cloud_sql::ip_choices(inst.as_ref());
        segmented_on(ui, &mut conf.ip, &opts);
        if !note.is_empty() {
            ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
        }
    });
    ui.end_row();
    form_label(ui, "IAM sign-in");
    ui.horizontal(|ui| {
        let was = conf.iam;
        let t = toggle(ui, &mut conf.iam);
        t.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, true, conf.iam, "Sign in with IAM"));
        // Turned on, the user becomes the gcloud account, which is usually also the one Application Default Credentials
        // were made with. The proxy cannot use the gcloud account for IAM, so the note says which one counts.
        if conf.iam && !was && let Some(a) = account {
            f.addr.user = crate::cloudsql::iam_user(a);
        }
        let note = if conf.iam { "No password. The proxy signs in with Application Default Credentials, which must be this user's account" } else { "The database user signs in with its password" };
        ui.label(RichText::new(note).color(MUTED()));
    });
    ui.end_row();
    form_label(ui, "User");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut f.addr.user).font(egui::TextStyle::Monospace).desired_width(200.0).margin(FIELD_MARGIN));
        let note = match (conf.iam, account) {
            (true, Some(a)) if f.addr.user.trim() == crate::cloudsql::iam_user(a) => "From gcloud, editable",
            (true, Some(_)) => "The IAM user to sign in as",
            (true, None) => "No gcloud account, so type the IAM user",
            (false, _) => "A database user of the instance",
        };
        ui.label(RichText::new(note).size(TXT_SMALL).color(FAINT()));
    });
    ui.end_row();
    if !conf.iam {
        form_label(ui, "Password");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut f.addr.password).password(true).font(egui::TextStyle::Monospace).desired_width(200.0).margin(FIELD_MARGIN));
            ui.label(RichText::new("Kept in the Keychain").size(TXT_SMALL).color(FAINT()));
        });
        ui.end_row();
    }
    form_label(ui, "Database");
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut f.addr.database).hint_text("postgres").desired_width(200.0).margin(FIELD_MARGIN));
        ui.label(RichText::new("Optional, empty opens postgres").size(TXT_SMALL).color(FAINT()));
    });
    ui.end_row();
    f.cloud_sql = Some(conf);
    if host {
        super::cloud_sql::use_host(f);
    }
    change
}

/// The Standard tab. True when Change asks for the Cloud SQL browser.
fn standard_tab(ui: &mut egui::Ui, f: &mut ConnForm, cloud: &super::cloud_sql::CloudCache) -> bool {
    let pg = f.engine == db::sql::Engine::Postgres;
    let cloud_on = f.cloud_on().is_some();
    let mut change = false;
    let mut paste = Paste::default();
    if pg && !cloud_on {
        let w = ui.available_width() - FORM_LABEL_W - FORM_GAP - 2.0 * ui.spacing().item_spacing.x;
        paste = paste_field(ui, f, w);
        ui.add_space(6.0);
    }
    settings_section(ui, "SERVER", |ui| {
        // Grid cells do not know their width on the first frame, so size fields from the section.
        let field_w = ui.available_width() - FORM_LABEL_W - FORM_GAP;
        egui::Grid::new("cf").num_columns(2).spacing([FORM_GAP, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
            form_label(ui, "Name");
            ui.add(egui::TextEdit::singleline(&mut f.name).hint_text("Production cluster").desired_width(field_w).margin(FIELD_MARGIN));
            ui.end_row();
            form_label(ui, "Tag");
            ui.vertical(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for t in std::iter::once("").chain(TAG_PRESETS) {
                        let c = if t.is_empty() { MUTED() } else { tag_color(t) };
                        if chip(ui, if t.is_empty() { "None" } else { t }, c, clean_tag(&f.tag).eq_ignore_ascii_case(t)).clicked() {
                            f.tag = t.to_string();
                        }
                    }
                });
                ui.add_space(4.0);
                ui.add(egui::TextEdit::singleline(&mut f.tag).hint_text("or type a custom tag").char_limit(24).desired_width(220.0).margin(FIELD_MARGIN));
            });
            ui.end_row();
            if cloud_on {
                change = cloud_rows(ui, f, cloud, field_w);
                return;
            }
            if !f.engine.is_sql() {
                form_label(ui, "URI");
                ui.vertical(|ui| {
                    let edit = if f.show { egui::TextEdit::multiline(&mut f.uri).desired_rows(3).return_key(None) } else { egui::TextEdit::singleline(&mut f.uri).password(true) };
                    ui.add(edit.font(egui::TextStyle::Monospace).hint_text("mongodb+srv://user:password@host/").desired_width(field_w).margin(FIELD_MARGIN_TALL));
                    // Wrapping is only visual; a pasted newline would break the URI.
                    if f.show {
                        f.uri.retain(|c| c != '\n' && c != '\r');
                    }
                    ui.add_space(2.0);
                    ui.checkbox(&mut f.show, "Show address");
                });
                ui.end_row();
                return;
            }
            form_label(ui, "Host");
            ui.horizontal(|ui| {
                let default_port = db::sql::Addr::default_port(&f.addr.scheme);
                let r = ui.add(egui::TextEdit::singleline(&mut f.addr.host).hint_text("localhost").desired_width(field_w - 100.0).margin(FIELD_MARGIN));
                filled_border(ui, &r, &mut paste.filled[PASTE_HOST]);
                ui.label("Port");
                let mut port = if f.addr.port == 0 { default_port.to_string() } else { f.addr.port.to_string() };
                let r = ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0).margin(FIELD_MARGIN));
                if r.changed() {
                    f.addr.port = port.trim().parse().unwrap_or(default_port);
                }
                filled_border(ui, &r, &mut paste.filled[PASTE_PORT]);
            });
            ui.end_row();
            form_label(ui, "User");
            let r = ui.add(egui::TextEdit::singleline(&mut f.addr.user).desired_width(200.0).margin(FIELD_MARGIN));
            filled_border(ui, &r, &mut paste.filled[PASTE_USER]);
            ui.end_row();
            form_label(ui, "Password");
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut f.addr.password).password(true).font(egui::TextStyle::Monospace).desired_width(200.0).margin(FIELD_MARGIN));
                filled_border(ui, &r, &mut paste.filled[PASTE_PASSWORD]);
                ui.label(RichText::new("Kept in the Keychain").size(TXT_SMALL).color(FAINT()));
            });
            ui.end_row();
            form_label(ui, "Database");
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut f.addr.database).hint_text(if pg { "postgres" } else { "All databases" }).desired_width(200.0).margin(FIELD_MARGIN));
                filled_border(ui, &r, &mut paste.filled[PASTE_DATABASE]);
                ui.label(RichText::new(if pg { "Optional, empty opens postgres" } else { "Optional, opens this one first" }).size(TXT_SMALL).color(FAINT()));
            });
            ui.end_row();
        });
        if cloud_on {
            ui.add_space(4.0);
            note_row(ui, C_LINK(), true, "Filled from Cloud SQL. The proxy starts on a free local port when you connect.");
        }
        if pg && paste.filled.iter().any(|b| *b) {
            ui.add_space(4.0);
            ui.label(RichText::new(if paste.filled[PASTE_TLS] { format!("Blue borders came from the string. The TLS tab now reads {}.", f.addr.ssl_mode.pg_name()) } else { "Blue borders came from the string.".to_string() }).size(TXT_SMALL).color(FAINT()));
        }
    });
    if pg && !cloud_on {
        ui.data_mut(|d| d.insert_temp(paste_id(f), paste));
    }
    change
}

fn ssh_tab(ui: &mut egui::Ui, f: &mut ConnForm) {
    settings_section(ui, "SSH TUNNEL", |ui| {
        if f.cloud_on().is_some() {
            f.ssh_on = false;
            ui.add_enabled(false, egui::Checkbox::new(&mut f.ssh_on, "Connect through an SSH server (jump host)"));
            ui.label(RichText::new("Not used with Cloud SQL: the proxy carries the connection. Use a host on the Standard tab to set a tunnel.").size(TXT_SMALL).color(FAINT()));
            return;
        }
        ui.checkbox(&mut f.ssh_on, "Connect through an SSH server (jump host)");
        if f.ssh_on {
            egui::Grid::new("ssh").num_columns(2).spacing([FORM_GAP, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                form_label(ui, "Host");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.ssh.host).hint_text("bastion.example.com").desired_width(280.0));
                    ui.label("Port");
                    let mut port = if f.ssh.port == 0 { "22".to_string() } else { f.ssh.port.to_string() };
                    if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                        f.ssh.port = port.trim().parse().unwrap_or(22);
                    }
                });
                ui.end_row();
                form_label(ui, "User");
                ui.add(egui::TextEdit::singleline(&mut f.ssh.user).desired_width(280.0));
                ui.end_row();
                form_label(ui, "Log in with");
                segmented(ui, &mut f.ssh.auth, &[(crate::ssh::SshAuth::Auto, "Auto"), (crate::ssh::SshAuth::Password, "Password"), (crate::ssh::SshAuth::KeyFile, "Key file"), (crate::ssh::SshAuth::Agent, "SSH agent")]);
                ui.end_row();
                use crate::ssh::SshAuth::*;
                if matches!(f.ssh.auth, Auto | Password) {
                    form_label(ui, "Password");
                    ui.add(egui::TextEdit::singleline(&mut f.ssh_pwd).password(true).hint_text(if f.ssh.auth == Auto { "optional, tried last" } else { "" }).desired_width(280.0));
                    ui.end_row();
                }
                if matches!(f.ssh.auth, Auto | KeyFile) {
                    form_label(ui, "Key file");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut f.ssh.key_path).hint_text(if f.ssh.auth == Auto { "optional; ~/.ssh/id_* are tried" } else { "~/.ssh/id_ed25519" }).desired_width(280.0));
                        if ui.button("Choose…").clicked() && let Some(p) = rfd::FileDialog::new().set_directory(dirs::home_dir().unwrap_or_default().join(".ssh")).pick_file() {
                            f.ssh.key_path = p.display().to_string();
                        }
                    });
                    ui.end_row();
                    form_label(ui, "Passphrase");
                    ui.add(egui::TextEdit::singleline(&mut f.ssh_pass).password(true).hint_text("if the key has one").desired_width(280.0));
                    ui.end_row();
                }
                ui.label("");
                ui.checkbox(&mut f.ssh.legacy_rsa, "Allow legacy ssh-rsa host keys (old servers)");
                ui.end_row();
                form_label(ui, "Host key");
                ui.horizontal(|ui| match f.ssh.known_host.clone() {
                    Some(fp) => {
                        ui.label(RichText::new(fp).monospace().size(TXT_SMALL).color(MUTED()));
                        if ui.button("Forget").on_hover_text("Trust the next key the server shows, e.g. after a reinstall").clicked() {
                            f.ssh.known_host = None;
                        }
                    }
                    None => {
                        ui.label(RichText::new("remembered on first connect; a different key later is refused").size(TXT_SMALL).color(FAINT()));
                    }
                });
                ui.end_row();
            });
        }
    });
}

fn proxy_tab(ui: &mut egui::Ui, f: &mut ConnForm) {
    settings_section(ui, "SOCKS5 PROXY", |ui| {
        ui.add_enabled(!f.ssh_on, egui::Checkbox::new(&mut f.proxy_on, "Connect through a SOCKS5 proxy"));
        if f.ssh_on {
            ui.label(RichText::new("Not needed with an SSH tunnel: the tunnel already carries the connection.").size(TXT_SMALL).color(FAINT()));
        } else if f.proxy_on {
            egui::Grid::new("px").num_columns(2).spacing([FORM_GAP, 10.0]).min_col_width(FORM_LABEL_W).show(ui, |ui| {
                form_label(ui, "Host");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut f.proxy.host).desired_width(280.0));
                    ui.label("Port");
                    let mut port = if f.proxy.port == 0 { "1080".to_string() } else { f.proxy.port.to_string() };
                    if ui.add(egui::TextEdit::singleline(&mut port).desired_width(60.0)).changed() {
                        f.proxy.port = port.trim().parse().unwrap_or(1080);
                    }
                });
                ui.end_row();
                form_label(ui, "User");
                ui.add(egui::TextEdit::singleline(&mut f.proxy.user).hint_text("optional").desired_width(280.0));
                ui.end_row();
                form_label(ui, "Password");
                ui.add(egui::TextEdit::singleline(&mut f.proxy_pwd).password(true).desired_width(280.0));
                ui.end_row();
            });
        }
    });
}

impl App {
    pub(super) fn conn_dialog(&mut self, ui: &mut egui::Ui, f: &mut ConnForm, keep: &mut bool) {
        if self.demo {
            paste_seed(ui.ctx(), f);
        }
        let mut save = false;
        let mut test = false;
        let mut browse = false;
        let default_s = self.prefs.timeout_ms / 1000;
        // An edited Cloud SQL connection shows the gcloud account and the instance's state, which the browser's load reads.
        if f.cloud_on().is_some() {
            self.cloud_load(false);
        }
        // Custom starts from what the server runs, once, so a mode the user already shaped is never replaced.
        if let Some((req, info)) = self.test_info.take() {
            f.take_test_info(req, info);
        }
        ui.set_width(760.0);
        dialog_title(ui, if f.id.is_some() { "Edit connection" } else { "New connection" });
        ui.label(RichText::new("Addresses, passwords and passphrases are stored in the macOS Keychain. The connection opens read-only.").size(TXT_SMALL).color(MUTED()));
        ui.add_space(8.0);
        let cloud = &self.cloud;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(210.0, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                let sql = f.engine.is_sql();
                let pg = f.engine == db::sql::Engine::Postgres;
                let maria = sql && !pg && f.addr.scheme == "mariadb";
                let mut pick = None;
                if engine_card(ui, "MO", "MongoDB", "mongodb://, Atlas SRV", C_STRING(), !sql, true).clicked() {
                    pick = Some((db::sql::Engine::Mongo, ""));
                }
                if engine_card(ui, "MY", "MySQL", "5.7, 8.x, 9.x", C_LINK(), sql && !maria && !pg, true).clicked() {
                    pick = Some((db::sql::Engine::MySql, "mysql"));
                }
                if engine_card(ui, "MA", "MariaDB", "10.6 and later", C_KEYWORD(), maria, true).clicked() {
                    pick = Some((db::sql::Engine::MySql, "mariadb"));
                }
                if engine_card(ui, "PG", "PostgreSQL", "13 and later", ENGINE_PG(), pg, true).clicked() {
                    pick = Some((db::sql::Engine::Postgres, "postgres"));
                }
                engine_card(ui, "SQ", "SQLite", "Coming later", FAINT(), false, false);
                if quiet(ui, "Add from Cloud SQL…").on_hover_text("Fill this form from a PostgreSQL instance your gcloud account can reach").clicked() {
                    browse = true;
                }
                if let Some((engine, scheme)) = pick {
                    f.engine = engine;
                    if engine.is_sql() {
                        // A port left at the old engine's default follows the new engine.
                        let (old, new) = (db::sql::Addr::default_port(&f.addr.scheme), db::sql::Addr::default_port(scheme));
                        if f.addr.port == 0 || f.addr.port == old {
                            f.addr.port = new;
                        }
                        // The libpq and mysql spellings of a mode differ, so a pick does not carry over.
                        if old != new {
                            f.addr.ssl_mode = Default::default();
                        }
                        f.addr.scheme = scheme.into();
                    }
                    // The other engine has no tab for the one that was open.
                    if !conn_tabs(engine).iter().any(|(t, _)| *t == f.tab) {
                        f.tab = ConnTab::Standard;
                    }
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    let cloud = f.cloud_on().is_some();
                    for &(t, label) in conn_tabs(f.engine) {
                        if view_tab_dim(ui, label, f.tab == t, cloud && matches!(t, ConnTab::Ssh | ConnTab::Tls)).clicked() {
                            f.tab = t;
                        }
                    }
                });
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() - 260.0).show(ui, |ui| match f.tab {
                    ConnTab::Standard => browse |= standard_tab(ui, f, cloud),
                    ConnTab::Ssh => ssh_tab(ui, f),
                    ConnTab::Proxy => proxy_tab(ui, f),
                    ConnTab::Tls => tls_tab(ui, f),
                    ConnTab::Advanced if f.engine == db::sql::Engine::Postgres => super::conn_pg_advanced::pg_advanced_tab(ui, f, default_s),
                    ConnTab::Advanced if f.engine.is_sql() => advanced_tab(ui, f, default_s),
                    ConnTab::Advanced => browse |= standard_tab(ui, f, cloud),
                });
            });
        });
        if f.engine == db::sql::Engine::Postgres {
            ui.add_space(6.0);
            egui::Frame::new().fill(BG()).corner_radius(R_MD).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                note_row(ui, PRIMARY(), true, "A locked PostgreSQL connection refuses writes Quarry can see. Functions on the server that write by themselves still run with your role's rights; use a read-only role for full safety.");
            });
        }
        // An answer for an older Test or another form is not this form's.
        if self.conn_test.as_ref().is_some_and(|(req, _)| *req != f.test_req) {
            self.conn_test = None;
        }
        match self.conn_test.as_ref().map(|(_, r)| r) {
            None if f.testing => {
                ui.horizontal(|ui| {
                    spin(ui);
                    ui.label(RichText::new("Testing…").color(MUTED()));
                });
            }
            Some(Ok(m)) => {
                ui.label(RichText::new(m).color(SUCCESS()));
            }
            Some(Err(e)) if e.contains(crate::cloudsql::PROXY_MISSING) => {}
            Some(Err(e)) => {
                // The driver's text for a client certificate problem names no file or fix; the explained words do.
                let text = db::client_cert_problem(e).map_or_else(|| e.clone(), |(t, h)| format!("{t}. {h}"));
                ui.add(egui::Label::new(RichText::new(text).color(ALERT())).wrap());
            }
            None => {}
        }
        if matches!(&self.conn_test, Some((_, Err(e))) if e.contains(crate::cloudsql::PROXY_MISSING)) {
            ui.add_space(6.0);
            match self.proxy_missing_card(ui, "Test") {
                Some(super::cloud_sql::CardAct::Again) => test = true,
                act => self.cloud_card_act(act, |_| {}),
            }
        }
        let adv_error = f.advanced_error();
        if let Some(m) = &adv_error {
            ui.add(egui::Label::new(RichText::new(m).color(ALERT())).wrap());
        }
        dialog_actions(ui, |ui| {
            let address_ok = if let Some(c) = f.cloud_on() {
                crate::cloudsql::valid_instance(&c.instance) && !f.addr.user.trim().is_empty()
            } else if f.engine.is_sql() { !f.addr.host.trim().is_empty() && !f.addr.user.trim().is_empty() } else { f.uri.starts_with("mongodb://") || f.uri.starts_with("mongodb+srv://") };
            let valid = adv_error.is_none() && !f.name.trim().is_empty() && address_ok && (!f.ssh_on || (!f.ssh.host.trim().is_empty() && !f.ssh.user.trim().is_empty())) && (!f.proxy_on || f.ssh_on || f.engine.is_sql() || !f.proxy.host.trim().is_empty());
            save = ui.add_enabled(valid, primary_button(ui, "Save and connect", PRIMARY())).clicked();
            test |= ui.add_enabled(valid, egui::Button::new("Test")).clicked();
            if ui.button("Cancel").clicked() {
                *keep = false;
            }
        });
        if !*keep {
            paste_reset(ui.ctx(), f);
        }
        if browse {
            self.open_cloud_browser(true);
        }
        if test {
            f.testing = true;
            self.conn_test = None;
            self.test_info = None;
            self.req += 1;
            f.test_req = self.req;
            let mut net = f.net();
            net.tools = self.cloud_tools();
            if f.engine.is_sql() {
                self.worker.sql_test_with(f.test_req, f.sql_addr().to_uri(), self.timeout(), net, f.advanced().unwrap_or_default());
            } else {
                self.worker.test(f.test_req, f.uri.trim().to_string(), self.timeout(), net);
            }
        }
        if save {
            let id = f.id.unwrap_or_else(|| self.conns.iter().map(|c| c.meta.id).max().unwrap_or(0) + 1);
            // Demo ids overlap real ones, so the demo keeps its edits in memory only.
            // SQL addresses are rebuilt from the fields, so the Keychain holds one URL for either engine.
            // A Cloud SQL IAM sign-in has no password, so none is stored with the address.
            let uri = if f.engine.is_sql() { f.sql_addr().to_uri() } else { f.uri.trim().to_string() };
            let saved = if self.ephemeral { Some(Ok(())) } else { keychain(id).map(|k| k.set_password(&uri)) };
            match saved {
                Some(Ok(())) => {
                    let stored = if self.ephemeral {
                        Vec::new()
                    } else {
                        vec![
                            set_secret(id, "ssh-password", if f.ssh_on { &f.ssh_pwd } else { "" }),
                            set_secret(id, "ssh-passphrase", if f.ssh_on { &f.ssh_pass } else { "" }),
                            set_secret(id, "proxy-password", if f.proxy_on && !f.ssh_on && !f.engine.is_sql() { &f.proxy_pwd } else { "" }),
                            set_secret(id, "pg-key-passphrase", if f.engine == db::sql::Engine::Postgres && f.cloud_on().is_none() && !f.addr.ssl_key.trim().is_empty() { &f.pg_key_pass } else { "" }),
                        ]
                    };
                    let meta = ConnMeta {
                        id,
                        engine: f.engine,
                        name: f.name.trim().into(),
                        tag: clean_tag(&f.tag).into(),
                        ssh: f.ssh_on.then(|| crate::ssh::SshConf { host: f.ssh.host.trim().into(), user: f.ssh.user.trim().into(), ..f.ssh.clone() }),
                        proxy: (f.proxy_on && !f.ssh_on && !f.engine.is_sql()).then(|| db::Proxy { host: f.proxy.host.trim().into(), password: None, ..f.proxy.clone() }),
                        cloud_sql: f.cloud_on().filter(|_| !f.ssh_on).cloned(),
                        advanced: f.advanced().unwrap_or_default(),
                    };
                    match self.conns.iter_mut().find(|c| c.meta.id == id) {
                        Some(c) => {
                            c.meta = meta;
                            c.masked_uri = db::mask_uri(&uri);
                            c.writable = false;
                            c.dbs.clear();
                            c.server = Default::default();
                        }
                        None => self.conns.push(Conn { masked_uri: db::mask_uri(&uri), ..Conn::new(meta, Status::Idle) }),
                    }
                    self.active = Some(id);
                    self.save_conns();
                    self.connect(id);
                    *keep = false;
                    if let Some(Err(e)) = stored.into_iter().find(|r| r.is_err()) {
                        self.err(e);
                    }
                }
                Some(Err(e)) => self.err(format!("Could not save the address to the Keychain: {e}")),
                None => self.err("The Keychain is not available."),
            }
        }
    }
}
