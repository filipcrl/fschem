//! Bounded, asynchronous, read-only file documents. No parser or execution hooks.
use eframe::egui;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

pub const EXTENSIONS: &[&str] = &[
    "py", "spice", "sim", "log", "lib", "va", "json", "toml", "txt",
];
const LIMIT: usize = 4 * 1024 * 1024;
const PAGE: usize = 100;
pub fn is_text(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| EXTENSIONS.iter().any(|e| s.eq_ignore_ascii_case(e)))
}
#[derive(Debug)]
struct Snapshot {
    text: String,
    lines: Vec<String>,
    truncated: bool,
    stamp: Option<(u64, SystemTime)>,
}
fn read(path: &Path) -> Result<Snapshot, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("Only regular text files are supported".into());
    }
    let stamp = metadata.modified().ok().map(|t| (metadata.len(), t));
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let truncated = bytes.len() > LIMIT;
    bytes.truncate(LIMIT);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let lines = text.split('\n').map(|s| s.to_owned()).collect();
    Ok(Snapshot {
        text,
        lines,
        truncated,
        stamp,
    })
}
pub struct TextDocument {
    pub path: PathBuf,
    snapshot: Option<Snapshot>,
    pending: Option<mpsc::Receiver<Result<Snapshot, String>>>,
    error: Option<String>,
    checked: Instant,
    pub follow: bool,
    wrap: bool,
    query: String,
    selected: usize,
    page: usize,
    goto: usize,
    reveal: bool,
}
impl TextDocument {
    pub fn new(ctx: &egui::Context, path: PathBuf) -> Self {
        let follow = path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("log"));
        let mut doc = Self {
            path,
            snapshot: None,
            pending: None,
            error: None,
            checked: Instant::now(),
            follow,
            wrap: false,
            query: String::new(),
            selected: 0,
            page: 0,
            goto: 1,
            reveal: false,
        };
        doc.reload(ctx);
        doc
    }
    pub fn ready(&self) -> bool {
        self.snapshot.is_some() && self.pending.is_none()
    }
    pub fn failed(&self) -> bool {
        self.error.is_some()
    }
    pub fn name(&self) -> String {
        format!(
            "{} [text]",
            self.path.file_name().unwrap_or_default().to_string_lossy()
        )
    }
    pub fn reload(&mut self, ctx: &egui::Context) {
        if self.pending.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.pending = Some(rx);
        let path = self.path.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read(&path));
            ctx.request_repaint();
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(result) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.pending = None;
            match result {
                Ok(snapshot) => {
                    let changed = self
                        .snapshot
                        .as_ref()
                        .is_none_or(|old| old.stamp != snapshot.stamp);
                    if self.follow && changed {
                        self.page = snapshot.lines.len().saturating_sub(1) / PAGE;
                    }
                    self.page = self.page.min(snapshot.lines.len().saturating_sub(1) / PAGE);
                    self.snapshot = Some(snapshot);
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        // Metadata and content are both checked off the UI thread, even on slow filesystems.
        if self.follow && self.checked.elapsed() >= Duration::from_secs(1) {
            self.checked = Instant::now();
            self.reload(ctx);
        }
        if self.follow || self.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }
    fn search(&mut self, previous: bool) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if self.query.is_empty() {
            return;
        }
        let n = snapshot.lines.len();
        for step in 1..=n {
            let i = if previous {
                (self.selected + n - step) % n
            } else {
                (self.selected + step) % n
            };
            if snapshot.lines[i].contains(&self.query) {
                self.selected = i;
                self.page = i / PAGE;
                self.goto = i + 1;
                self.reveal = true;
                return;
            }
        }
    }
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll(ui.ctx());
        ui.horizontal_wrapped(|ui| {
            if ui.button("Reload").clicked() {
                self.reload(ui.ctx());
            }
            ui.checkbox(&mut self.follow, "Follow");
            ui.checkbox(&mut self.wrap, "Wrap");
            if ui.button("Copy loaded text").clicked()
                && let Some(s) = &self.snapshot {
                ui.ctx().copy_text(s.text.clone());
            }
            if self.pending.is_some() {
                ui.spinner();
            }
            ui.label("Read-only");
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Find (case-sensitive):");
            ui.text_edit_singleline(&mut self.query);
            if ui.button("Previous match").clicked() {
                self.search(true);
            }
            if ui.button("Next match").clicked() {
                self.search(false);
            }
            ui.add(egui::DragValue::new(&mut self.goto).range(1..=usize::MAX));
            if ui.button("Go to line").clicked() {
                self.selected = self
                    .goto
                    .saturating_sub(1)
                    .min(self.snapshot.as_ref().map_or(0, |s| s.lines.len() - 1));
                self.page = self.selected / PAGE;
                self.reveal = true;
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, e);
        }
        let Some(s) = &self.snapshot else {
            return;
        };
        if s.truncated {
            ui.label(
                "Showing first 4 MiB only; reload/follow do not show the tail beyond this limit.",
            );
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.page > 0, egui::Button::new("Previous page"))
                .clicked()
            {
                self.page -= 1;
            }
            if ui
                .add_enabled(
                    (self.page + 1) * PAGE < s.lines.len(),
                    egui::Button::new("Next page"),
                )
                .clicked()
            {
                self.page += 1;
            }
            ui.label(format!(
                "{} lines · page {} / {}",
                s.lines.len(),
                self.page + 1,
                s.lines.len().div_ceil(PAGE)
            ));
        });
        egui::ScrollArea::both()
            .id_salt(("text", &self.path, self.page))
            .show(ui, |ui| {
                for (i, line) in s.lines.iter().enumerate().skip(self.page * PAGE).take(PAGE) {
                    ui.horizontal_top(|ui| {
                        ui.monospace(format!("{:>7}", i + 1));
                        // Bound layout work for pathological single-line files; copying retains all loaded bytes.
                        let end = line.char_indices().nth(4096).map_or(line.len(), |(i, _)| i);
                        let text = egui::RichText::new(&line[..end]).monospace();
                        let text = if i == self.selected {
                            text.background_color(ui.visuals().selection.bg_fill)
                        } else {
                            text
                        };
                        let response = ui.add(egui::Label::new(text).selectable(true).wrap_mode(
                            if self.wrap {
                                egui::TextWrapMode::Wrap
                            } else {
                                egui::TextWrapMode::Extend
                            },
                        ));
                        if self.reveal && i == self.selected {
                            response.scroll_to_me(Some(egui::Align::Center));
                            self.reveal = false;
                        }
                        if end < line.len() {
                            ui.label("[… line display limited to 4096 characters]");
                        }
                    });
                }
            });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dispatch() {
        for ext in EXTENSIONS {
            assert!(is_text(Path::new(&format!("file.{ext}"))));
        }
        assert!(is_text(Path::new("TEST.PY")));
        assert!(!is_text(Path::new("test.sch")));
        assert!(!is_text(Path::new("test.raw")));
    }
    #[test]
    fn asynchronous_reload_search_and_change() {
        let dir = std::env::temp_dir().join(format!("fschem-text-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.txt");
        std::fs::write(&path, "first\nmatch\nlast match").unwrap();
        let ctx = egui::Context::default();
        let mut doc = TextDocument::new(&ctx, path.clone());
        fn wait(doc: &mut TextDocument, ctx: &egui::Context) {
            for _ in 0..500 {
                doc.poll(ctx);
                if doc.pending.is_none() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("read timed out");
        }
        wait(&mut doc, &ctx);
        doc.query = "match".into();
        doc.search(false);
        assert_eq!(doc.selected, 1);
        doc.search(true);
        assert_eq!(doc.selected, 2);
        std::fs::write(&path, "replacement").unwrap();
        doc.follow = true;
        doc.checked = Instant::now() - Duration::from_secs(2);
        doc.poll(&ctx);
        wait(&mut doc, &ctx);
        assert_eq!(doc.snapshot.as_ref().unwrap().text, "replacement");
        std::fs::write(&path, vec![b'a'; LIMIT + 10]).unwrap();
        assert!(read(&path).unwrap().truncated);
        std::fs::remove_file(&path).unwrap();
        doc.reload(&ctx);
        wait(&mut doc, &ctx);
        assert!(doc.error.is_some());
        assert_eq!(doc.snapshot.as_ref().unwrap().text, "replacement");
        std::fs::remove_dir(dir).unwrap();
    }
}
