//! Native child viewport for simulation results, with an embedded-window fallback.
use crate::graph::Display;
use eframe::egui::{self, Color32};
use fschem::raw::RawFile;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GraphTarget {
    pub document: u64,
    pub record: u64,
}
struct GraphLink {
    target: GraphTarget,
    properties: fschem::document::Properties,
    pending: std::collections::BTreeMap<String, String>,
}

#[derive(Default)]
pub struct Waveforms {
    state: Arc<Mutex<WindowState>>,
}
fn file_stamp(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let metadata = path.metadata().ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}
fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("fschem-waveforms")
}
impl Waveforms {
    #[cfg(test)]
    pub(crate) fn test_link(&self, edit: Option<(&str, &str)>) -> Option<GraphTarget> {
        let mut state = self.state.lock().unwrap();
        let link = state.link.as_mut()?;
        if let Some((key, value)) = edit { link.pending.insert(key.into(), value.into()); }
        Some(link.target)
    }

    pub(crate) fn open_graph(&mut self, ctx: &egui::Context, target: GraphTarget, path: PathBuf, properties: fschem::document::Properties) {
        let mut state = self.state.lock().unwrap();
        state.data = None;
        state.load_selected(ctx, path, None);
        state.link = Some(GraphLink { target, properties, pending: Default::default() });
        state.detached = false;
    }
    /// The owner validates identity and the source snapshot before accepting queued changes.
    pub(crate) fn sync_graph(&mut self, ctx: &egui::Context, mut sync: impl FnMut(GraphTarget, &fschem::document::Properties, std::collections::BTreeMap<String, String>) -> Option<(PathBuf, fschem::document::Properties)>) {
        let mut state = self.state.lock().unwrap();
        let Some(mut link) = state.link.take() else { return; };
        if !state.open { return; }
        let Some((path, properties)) = sync(link.target, &link.properties, std::mem::take(&mut link.pending)) else {
            state.detached = true;
            return;
        };
        if state.watched.as_ref() != Some(&path) {
            state.data = None;
            state.load_selected(ctx, path, None);
        } else if properties.raw != link.properties.raw {
            if let Some(data) = &mut state.data { data.configure(&properties); }
        }
        link.properties = properties;
        state.link = Some(link);
    }

    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        self.state.lock().unwrap().open_dialog(ctx);
    }
    pub fn load(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.state.lock().unwrap().load(ctx, path);
    }
    pub fn load_plot(&mut self, ctx: &egui::Context, path: PathBuf, plot: usize) {
        let mut state = self.state.lock().unwrap();
        state.link = None;
        state.detached = false;
        state.data = None; // Explicit analysis selection must not restore another plot.
        state.load_selected(ctx, path, Some(plot));
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        let title = {
            let mut state = self.state.lock().unwrap();
            state.poll();
            if !state.open {
                return;
            }
            state
                .data
                .as_ref()
                .map(|d| {
                    format!(
                        "{} — Waveforms",
                        d.path.file_name().unwrap_or_default().to_string_lossy()
                    )
                })
                .unwrap_or_else(|| "Waveforms — fschem".into())
        };
        let shared = Arc::clone(&self.state);
        // Deferred viewports repaint independently: moving the waveform cursor
        // no longer rebuilds/tessellates the schematic in the parent window.
        ctx.show_viewport_deferred(
            viewport_id(),
            egui::ViewportBuilder::default()
                .with_title(&title)
                .with_inner_size([1050., 700.])
                .with_min_inner_size([650., 400.]),
            move |ctx, class| {
                let mut state = shared.lock().unwrap();
                state.poll();
                if class == egui::ViewportClass::Embedded {
                    let mut open = state.open;
                    egui::Window::new(&title)
                        .id(egui::Id::new("waveform-fallback"))
                        .open(&mut open)
                        .default_size([950., 600.])
                        .show(ctx, |ui| state.contents(ui));
                    state.open = open;
                } else if ctx.input(|i| i.viewport().close_requested()) {
                    state.open = false;
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                } else if state.open {
                    egui::CentralPanel::default().show(ctx, |ui| state.contents(ui));
                }
            },
        );
    }
}

#[derive(Default)]
struct WindowState {
    link: Option<GraphLink>,
    detached: bool,
    open: bool,
    data: Option<Display>,
    loading: Option<mpsc::Receiver<Result<Display, String>>>,
    error: Option<String>,
    watched: Option<PathBuf>,
    selected_plot: Option<usize>,
    stamp: Option<(std::time::SystemTime, u64)>,
    checked: Option<std::time::Instant>,
}
impl WindowState {
    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open simulation raw data")
            .add_filter("Simulation raw data", &["raw"]);
        if let Some(directory) = self.data.as_ref().and_then(|d| d.path.parent()) {
            dialog = dialog.set_directory(directory);
        }
        if let Some(path) = dialog.pick_file() {
            self.load(ctx, path);
        }
    }

    pub fn load(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.link = None;
        self.detached = false;
        self.load_selected(ctx, path, None);
    }
    fn load_selected(&mut self, ctx: &egui::Context, path: PathBuf, plot: Option<usize>) {
        self.selected_plot = plot;
        if plot.is_some() {
            // Never retain a different result behind a failed explicit selection.
            self.data = None;
        }
        self.watched = Some(path.clone());
        self.stamp = file_stamp(&path);
        self.checked = Some(std::time::Instant::now());
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx); // Dropping the previous receiver cancels stale loads.
        self.error = None;
        self.open = true;
        let background_ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = RawFile::open(&path).and_then(|raw| {
                let mut display = Display::new(path, raw);
                if let Some(index) = plot { display.open_plot(index)?; }
                Ok(display)
            });
            let _ = tx.send(result);
            background_ctx.request_repaint_of(viewport_id());
            background_ctx.request_repaint_of(egui::ViewportId::ROOT);
        });
        ctx.request_repaint();
    }

    fn poll(&mut self) {
        if let Some(result) = self.loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.loading = None;
            match result {
                Ok(mut data) => {
                    if self.selected_plot.is_none()
                        && let Some(previous) = &self.data
                        && previous.path == data.path
                    {
                        data.restore_view(previous);
                    }
                    if let Some(link) = &self.link { data.configure(&link.properties); }
                    self.data = Some(data);
                }
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
        if self.loading.is_none()
            && self
                .checked
                .is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(500))
        {
            self.checked = Some(std::time::Instant::now());
            if let Some(path) = self.watched.clone() {
                let next = file_stamp(&path);
                if next.is_some() && (next != self.stamp || self.error.is_some()) {
                    self.load_selected(ui.ctx(), path, self.selected_plot);
                } else if next.is_none() {
                    self.stamp = None;
                }
            }
        }
        ui.horizontal(|ui| {
            if ui.button("Open raw…").clicked() {
                self.open_dialog(ui.ctx());
            }
            if ui
                .add_enabled(
                    self.data.is_some() && self.loading.is_none(),
                    egui::Button::new("Reload"),
                )
                .clicked()
            {
                let path = self.data.as_ref().unwrap().path.clone();
                self.load_selected(ui.ctx(), path, self.selected_plot);
            }
            if self.loading.is_some() {
                ui.spinner();
                ui.label("Loading raw data…");
            }
        });
        if let Some(error) = &self.error {
            ui.colored_label(Color32::LIGHT_RED, error);
        }
        let Some(data) = &mut self.data else {
            ui.label("Open a raw file, or run a simulation from the schematic toolbar.");
            return;
        };
        if self.link.is_some() { ui.weak("Linked schematic graph · edits update the document (not disk)"); }
        if self.detached { ui.weak("Unlinked graph · source closed, reloaded or removed. Press E on a graph to reconnect."); }
        let before = data.configuration();
        let before_range = data.x_range();
        data.ui(ui);
        let after = data.configuration();
        if let Some(link) = &mut self.link {
            if before_range.is_some() && before_range != data.x_range() {
                if let Some((a, b)) = data.x_range() {
                    link.pending.insert("x1".into(), a.to_string());
                    link.pending.insert("x2".into(), b.to_string());
                }
            }
            for (key, value) in after {
                if before.get(&key) != Some(&value) { link.pending.insert(key, value); }
            }
            if !link.pending.is_empty() { ui.ctx().request_repaint_of(egui::ViewportId::ROOT); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linked_load_refresh_and_detach_keep_data_without_feedback() {
        let path = std::env::temp_dir().join(format!("fschem-linked-window-{}.raw", std::process::id()));
        std::fs::write(&path, b"Title: test\nPlotname: Transient\nFlags: real\nNo. Variables: 3\nNo. Points: 2\nVariables:\n0 time time\n1 v(out) voltage\n2 i(v1) current\nValues:\n0 0\n2\n0.01\n1 1\n3\n0.02\n").unwrap();
        let ctx = egui::Context::default();
        let mut window = Waveforms::default();
        let target = GraphTarget { document: 10, record: 20 };
        let properties = fschem::document::Properties::parse("node=i(v1) groupmode=separate x1=0.2 x2=0.8".into());
        window.open_graph(&ctx, target, path.clone(), properties.clone());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let mut state = window.state.lock().unwrap();
            state.poll();
            if state.loading.is_none() { break; }
            assert!(std::time::Instant::now() < deadline);
            drop(state);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        {
            let state = window.state.lock().unwrap();
            let data = state.data.as_ref().unwrap();
            assert_eq!(data.configuration()["node"], "i(v1)");
            assert_eq!(data.configuration()["groupmode"], "separate");
            assert_eq!(data.x_range(), Some((0.2, 0.8)));
        }
        let updated = fschem::document::Properties::parse("node=out groupmode=overlay".into());
        window.sync_graph(&ctx, |owner, previous, edits| {
            assert_eq!(owner, target);
            assert_eq!(previous.raw, properties.raw);
            assert!(edits.is_empty(), "opening a graph must not mark it dirty");
            Some((path.clone(), updated.clone()))
        });
        {
            let state = window.state.lock().unwrap();
            assert_eq!(state.data.as_ref().unwrap().configuration()["node"], "v(out)");
            assert!(state.link.as_ref().unwrap().pending.is_empty());
        }
        window.test_link(Some(("node", "queued")));
        window.sync_graph(&ctx, |_, _, _| None);
        let state = window.state.lock().unwrap();
        assert!(state.detached);
        assert!(state.link.is_none());
        assert!(state.data.is_some(), "detachment retains inspectable data");
        drop(state);
        window.sync_graph(&ctx, |_, _, _| panic!("detached updates must not reach the owner"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn explicit_invalid_plot_stays_unavailable_across_retry() {
        let path = std::env::temp_dir().join(format!("fschem-plot-selection-{}.raw", std::process::id()));
        std::fs::write(&path, b"Title: divider\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n0 v(out) voltage\nValues:\n0 2\n").unwrap();
        let ctx = egui::Context::default();
        let mut state = WindowState::default();
        state.load_selected(&ctx, path.clone(), Some(5));
        for attempt in 0..2 {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while state.loading.is_some() {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(10));
                state.poll();
            }
            assert!(state.error.is_some());
            assert!(state.data.is_none());
            assert_eq!(state.selected_plot, Some(5));
            if attempt == 1 {
                break;
            }
            state.checked = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| state.contents(ui));
            });
            assert!(state.loading.is_some());
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn waveform_window_renders_in_embedded_viewports() {
        let raw = RawFile::parse(b"Title: divider\nPlotname: Operating Point\nFlags: real\nNo. Variables: 2\nNo. Points: 1\nVariables:\n0 v(out) voltage\n1 i(v1) current\nValues:\n0 2\n-1e-4\n").unwrap();
        let state = WindowState {
            open: true,
            data: Some(Display::new("divider.raw".into(), raw)),
            ..Default::default()
        };
        let mut waveforms = Waveforms {
            state: Arc::new(Mutex::new(state)),
        };
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200., 850.),
            )),
            ..Default::default()
        };
        // A newly opened egui Window uses a sizing pass before painting.
        let _ = ctx.run(input.clone(), |ctx| waveforms.show(ctx));
        let output = ctx.run(input, |ctx| waveforms.show(ctx));
        assert!(
            !ctx.tessellate(output.shapes, output.pixels_per_point)
                .is_empty()
        );
        assert!(waveforms.state.lock().unwrap().data.is_some());
    }

    #[test]
    fn native_window_registers_an_independent_repaint_callback() {
        let ctx = egui::Context::default();
        ctx.set_embed_viewports(false);
        let mut waveforms = Waveforms::default();
        waveforms.state.lock().unwrap().open = true;
        let output = ctx.run(egui::RawInput::default(), |ctx| waveforms.show(ctx));
        let child = &output.viewport_output[&viewport_id()];
        assert!(child.viewport_ui_cb.is_some());
        // Registering a native child does not paint it into the parent viewport.
        assert!(output.shapes.is_empty());
    }
}
