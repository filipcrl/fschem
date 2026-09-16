//! Native child viewport for simulation results, with an embedded-window fallback.
use crate::graph::Display;
use eframe::egui::{self, Color32};
use fschem::raw::RawFile;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
};

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
    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        self.state.lock().unwrap().open_dialog(ctx);
    }
    pub fn load(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.state.lock().unwrap().load(ctx, path);
    }
    pub fn load_plot(&mut self, ctx: &egui::Context, path: PathBuf, plot: usize) {
        let mut state = self.state.lock().unwrap();
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
        data.ui(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
