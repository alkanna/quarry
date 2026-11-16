//! A connection's own rail icon: an image picked from disk or fetched from a URL, cropped to its centered square and
//! scaled to one small PNG in the config folder, `icons/<id>.png`. `ConnMeta::icon` says whether one is set, so a
//! connection without one never touches the disk and a demo connection, whose id may be a real one's, never reads it.

use super::rail::{engine_mark, logo, mark_color};
use super::*;
use std::sync::mpsc;

/// The stored icon's side: a 40 pt tile at 2x and 125 %, rounded up.
const SIDE: u32 = 128;
/// Files and downloads past this are refused before decoding.
const MAX_BYTES: u64 = 10 << 20;
/// What the image crate is built to read, for the file picker and the error wording.
const EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"];
const FORMATS: &str = "PNG, JPEG, GIF, WebP, BMP or ICO";

type Job = mpsc::Receiver<Result<Vec<u8>, String>>;

fn icons_dir() -> std::path::PathBuf {
    config_dir().join("icons")
}

pub(super) fn path(id: u64) -> std::path::PathBuf {
    icons_dir().join(format!("{id}.png"))
}

/// Decodes any supported image, crops it to its centered square and scales it to `SIDE`, as PNG bytes.
pub(super) fn square_png(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("The image is larger than 10 MB.".into());
    }
    let mut r = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    if r.format().is_none() {
        return Err(format!("Not an image Quarry can read. Use {FORMATS}."));
    }
    // A small file can claim a huge canvas; these keep the decode to a few hundred MB at most.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 << 20);
    r.limits(limits);
    let img = r.decode().map_err(|e| format!("Could not read the image: {e}"))?;
    let img = img.resize_to_fill(SIDE, SIDE, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    img.to_rgba8().write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Downloads an image with the system's curl, as the updater does, so the binary needs no HTTP client.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Enter an address starting with https:// or http://.".into());
    }
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-fsSL", "--proto", "=http,https", "--proto-redir", "=http,https", "--max-time", "20", "--max-filesize", &MAX_BYTES.to_string(), "-A", "quarry"]).arg(url);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, crate::update::CREATE_NO_WINDOW);
    let mut child = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().map_err(|e| format!("Could not run curl: {e}"))?;
    // curl's --max-filesize cannot stop a body sent without a length, so the read itself stops past the limit.
    let body = child.stdout.take().map(|o| capped(o)).transpose();
    let body = match body {
        Ok(Some(b)) => b,
        Ok(None) => Vec::new(),
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let out = child.wait_with_output().map_err(|e| format!("Could not run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("Could not download the image. {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(body)
}

/// Reads at most `MAX_BYTES`, failing past it, so a stream that never ends cannot fill memory.
fn capped(r: impl std::io::Read) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(r, MAX_BYTES + 1), &mut buf).map_err(|e| e.to_string())?;
    if buf.len() as u64 > MAX_BYTES {
        return Err("The image is larger than 10 MB.".into());
    }
    Ok(buf)
}

fn read_file(p: &std::path::Path) -> Result<Vec<u8>, String> {
    let file = std::fs::File::open(p).map_err(|e| e.to_string())?;
    // A pipe or a device reports no length and never ends; only a regular file is read.
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("That is not an image file.".into());
    }
    capped(file)
}

/// Runs `load` and the resize off the UI thread, since a large photo takes a while to decode.
fn spawn(ctx: &egui::Context, load: impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static) -> Job {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(load().and_then(|b| square_png(&b)));
        ctx.request_repaint();
    });
    rx
}

fn texture_of(ctx: &egui::Context, png: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(ctx.load_texture("conn-icon", egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR))
}

fn cache_id(id: u64) -> egui::Id {
    egui::Id::unique(("conn-icon", id))
}

/// The connection's icon, read from disk once and kept in egui's memory, a missing file included. None without one.
pub(super) fn texture(ctx: &egui::Context, meta: &ConnMeta) -> Option<egui::TextureHandle> {
    if !meta.icon {
        return None;
    }
    let key = cache_id(meta.id);
    if let Some(t) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(key)) {
        return t;
    }
    let t = std::fs::read(path(meta.id)).ok().and_then(|b| texture_of(ctx, &b));
    ctx.data_mut(|d| d.insert_temp(key, t.clone()));
    t
}

/// The icon filling `tile`, rounded like the engine tiles. `tint` dims it under a spinner.
pub(super) fn paint(p: &egui::Painter, tile: egui::Rect, tex: &egui::TextureHandle, tint: Color32) {
    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    p.add(egui::epaint::RectShape::filled(tile, R_TILE, tint).with_texture(tex.id(), uv));
}

/// The Connection icon dialog's working copy. Nothing is written until Save.
pub(super) struct IconForm {
    conn: u64,
    url: String,
    /// What Save writes: a new PNG, or an empty one to go back to the engine logo. None until something is picked.
    staged: Option<Vec<u8>>,
    preview: Option<egui::TextureHandle>,
    /// The file or download being read. It belongs to this dialog, so a late answer never lands on another connection.
    job: Option<Job>,
    error: String,
}

impl IconForm {
    pub(super) fn new(conn: u64) -> Self {
        Self { conn, url: String::new(), staged: None, preview: None, job: None, error: String::new() }
    }
}

impl App {
    pub(super) fn icon_dialog(&mut self, ui: &mut egui::Ui, f: &mut IconForm, keep: &mut bool) {
        let ctx = ui.ctx().clone();
        if let Some(res) = f.job.as_ref().and_then(|j| j.try_recv().ok()) {
            f.job = None;
            match res {
                Ok(png) => {
                    f.preview = texture_of(&ctx, &png);
                    f.staged = Some(png);
                    f.error.clear();
                }
                Err(e) => f.error = e,
            }
        }
        let Some(c) = self.conn(f.conn) else {
            *keep = false;
            return;
        };
        let (mark, _) = engine_mark(c.meta.engine, &c.server.product);
        let current = texture(&ctx, &c.meta);
        let has_icon = c.meta.icon;
        ui.set_width(fit_width(&ctx, 460.0));
        dialog_head(ui, &Head::new("Connection icon").sub(c.meta.name.clone()).glyph(glyphs::pencil).tone(PRIMARY()));
        let busy = f.job.is_some();
        let mut pick = false;
        let mut fetch_now = false;
        dialog_body(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), Sense::hover());
                let p = ui.painter();
                let shown = match &f.staged {
                    Some(png) if png.is_empty() => None,
                    Some(_) => f.preview.clone(),
                    None => current,
                };
                match shown {
                    Some(t) => paint(p, rect, &t, Color32::WHITE),
                    None => {
                        gradient_rect(p, rect, R_TILE as f32, tint(mark_color(mark), 140));
                        logo(p, rect.center(), 34.0, mark, Color32::WHITE);
                    }
                }
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(6.0);
                    if ui.add_enabled(!busy, egui::Button::new("Choose image\u{2026}")).clicked() {
                        pick = true;
                    }
                    ui.add_space(4.0);
                    ui.label(RichText::new(format!("{FORMATS}. Cropped to a square and scaled to {SIDE} px.")).size(TXT_SMALL).color(FAINT()));
                });
            });
            ui.add_space(14.0);
            form_card(ui, "From the web", "", |rows| {
                rows.row("Image URL", "", |ui| {
                    if ui.add_enabled(!busy && !f.url.trim().is_empty(), egui::Button::new("Fetch")).clicked() {
                        fetch_now = true;
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut f.url).hint_text(RichText::new("https://example.com/logo.png").color(FAINT())).desired_width(ui.available_width()).margin(egui::Margin { left: 10, right: 10, top: 5, bottom: 5 }));
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::Role::TextInput, true, "Image URL"));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !busy && !f.url.trim().is_empty() {
                        fetch_now = true;
                    }
                });
            });
            if busy {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(RichText::new("Reading the image\u{2026}").color(MUTED()));
                });
            } else if !f.error.is_empty() {
                banner(ui, Notice::Danger, "Could not use this image.", &f.error, None);
            }
        });
        if pick && let Some(p) = rfd::FileDialog::new().add_filter("Images", EXTS).pick_file() {
            f.job = Some(spawn(&ctx, move || read_file(&p)));
        }
        if fetch_now {
            let url = f.url.trim().to_string();
            f.job = Some(spawn(&ctx, move || fetch(&url)));
        }
        let mut save = false;
        dialog_foot(ui, |ui| {
            if ui.add_enabled_ui(f.staged.is_some() && !busy, |ui| primary(ui, "Save", PRIMARY())).inner.clicked() {
                save = true;
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                *keep = false;
            }
            let custom = matches!(&f.staged, Some(png) if !png.is_empty()) || (has_icon && f.staged.is_none());
            foot_left(ui, |ui| {
                if ui.add_enabled(custom && !busy, egui::Button::new("Use engine logo")).clicked() {
                    f.staged = Some(Vec::new());
                    f.preview = None;
                }
            });
        });
        if save {
            self.save_icon(&ctx, f);
            *keep = false;
        }
    }

    fn save_icon(&mut self, ctx: &egui::Context, f: &mut IconForm) {
        let Some(png) = f.staged.take() else { return };
        let (id, on) = (f.conn, !png.is_empty());
        // The demo keeps its icon in memory only, since its ids may be real connections' ids.
        if !self.ephemeral {
            let res = if on {
                write_atomic(&icons_dir(), &format!("{id}.png"), &png)
            } else {
                std::fs::remove_file(path(id)).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) })
            };
            if let Err(e) = res {
                return self.err(format!("Could not save the icon: {e}"));
            }
        }
        ctx.data_mut(|d| d.insert_temp(cache_id(id), f.preview.take().filter(|_| on)));
        if let Some(c) = self.conns.iter_mut().find(|c| c.meta.id == id) {
            c.meta.icon = on;
        }
        self.save_conns();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_image_becomes_a_small_square_png() {
        let mut wide = Vec::new();
        image::RgbaImage::new(300, 100).write_to(&mut std::io::Cursor::new(&mut wide), image::ImageFormat::Png).unwrap();
        let out = image::load_from_memory(&square_png(&wide).unwrap()).unwrap();
        assert_eq!((out.width(), out.height()), (SIDE, SIDE));
        assert!(square_png(b"not an image").is_err());
        assert!(fetch("file:///etc/passwd").is_err());
    }
}
