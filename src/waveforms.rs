//! Native child viewport for ngspice results, with an embedded-window fallback.
use crate::waveform_lod::{PlotBounds, PlotPoint, Series, View};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use fschem::raw::{RawFile, RawPlot, Sample};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

#[derive(Default)]
pub struct Waveforms {
    state: Arc<Mutex<WindowState>>,
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
}
impl WindowState {
    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new()
            .set_title("Open ngspice raw data")
            .add_filter("ngspice raw data", &["raw"]);
        if let Some(directory) = self.data.as_ref().and_then(|d| d.path.parent()) {
            dialog = dialog.set_directory(directory);
        }
        if let Some(path) = dialog.pick_file() {
            self.load(ctx, path);
        }
    }

    pub fn load(&mut self, ctx: &egui::Context, path: PathBuf) {
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx); // Dropping the previous receiver cancels stale loads.
        self.error = None;
        self.open = true;
        let background_ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = RawFile::open(&path).map(|raw| Display::new(path, raw));
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
                Ok(data) => self.data = Some(data),
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
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
                self.load(ui.ctx(), path);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Real,
    Imaginary,
    Magnitude,
    Decibels,
    Phase,
}
impl Mode {
    const ALL: [Self; 5] = [
        Self::Real,
        Self::Imaginary,
        Self::Magnitude,
        Self::Decibels,
        Self::Phase,
    ];
    fn label(self) -> &'static str {
        match self {
            Self::Real => "Real",
            Self::Imaginary => "Imaginary",
            Self::Magnitude => "Magnitude",
            Self::Decibels => "Magnitude (dB)",
            Self::Phase => "Phase (degrees)",
        }
    }
    fn value(self, sample: Sample) -> f64 {
        match self {
            Self::Real => sample.real,
            Self::Imaginary => sample.imaginary,
            Self::Magnitude => sample.real.hypot(sample.imaginary),
            Self::Decibels => 20. * sample.real.hypot(sample.imaginary).log10(),
            Self::Phase => sample.imaginary.atan2(sample.real).to_degrees(),
        }
    }
}

struct Trace {
    index: usize,
    label: String,
    series: Series,
    omitted: usize,
    overview_view: Option<View>,
    overview: Vec<Vec<PlotPoint>>,
}

struct Preparation {
    receiver: mpsc::Receiver<Vec<Trace>>,
    cancelled: Arc<AtomicBool>,
}
impl Drop for Preparation {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}
impl Trace {
    fn new(plot: &RawPlot, index: usize, mode: Mode, log_x: bool) -> Self {
        let (segments, omitted) = trace_points(plot, index, mode, log_x);
        let variable = &plot.variables[index];
        let units = match mode {
            Mode::Phase => "degrees",
            Mode::Decibels => "dB",
            _ => unit(&variable.kind),
        };
        Self {
            index,
            label: format!("{} [{units}]", variable.name),
            series: Series::new(segments),
            omitted,
            overview_view: None,
            overview: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupMode {
    Overlay,
    ByUnit,
    Separate,
}

struct Display {
    path: PathBuf,
    raw: Arc<RawFile>,
    plot: usize,
    selected: Vec<bool>,
    filter: String,
    mode: Mode,
    log_x: bool,
    fit: bool,
    x_range: Option<(f64, f64)>,
    group_mode: GroupMode,
    dirty: bool,
    traces: BTreeMap<usize, Trace>,
    projection: (usize, Mode, bool),
    preparing: Option<Preparation>,
    filtered: Vec<usize>,
    last_filter: String,
    omitted: usize,
}
impl Display {
    fn new(path: PathBuf, raw: RawFile) -> Self {
        let mut result = Self {
            path,
            raw: Arc::new(raw),
            plot: 0,
            selected: Vec::new(),
            filter: String::new(),
            mode: Mode::Real,
            log_x: false,
            fit: true,
            x_range: None,
            group_mode: GroupMode::Overlay,
            dirty: true,
            traces: BTreeMap::new(),
            projection: (0, Mode::Real, false),
            preparing: None,
            filtered: Vec::new(),
            last_filter: "\0".into(),
            omitted: 0,
        };
        result.select_plot();
        // Construction runs in the file-loading worker, including projection
        // and extrema indexing for the initially selected trace.
        result.projection = (result.plot, result.mode, result.log_x);
        for (i, selected) in result.selected.iter().enumerate() {
            if *selected {
                result.traces.insert(
                    i,
                    Trace::new(&result.raw.plots[result.plot], i, result.mode, result.log_x),
                );
            }
        }
        result.dirty = false;
        result
    }
    fn select_plot(&mut self) {
        let plot = &self.raw.plots[self.plot];
        self.mode = if plot.complex {
            Mode::Magnitude
        } else {
            Mode::Real
        };
        self.log_x = plot.variables[0].kind.eq_ignore_ascii_case("frequency");
        self.selected = vec![false; plot.variables.len()];
        if let Some(index) = (0..plot.variables.len()).find(|i| Some(*i) != plot.scale()) {
            self.selected[index] = true;
        }
        self.filter.clear();
        self.last_filter = "\0".into();
        self.dirty = true;
        self.fit = true;
        self.x_range = None;
    }
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.label(self.path.display().to_string());
        let old_plot = self.plot;
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("raw-plot")
                .selected_text(format!(
                    "{}: {}",
                    self.plot + 1,
                    self.raw.plots[self.plot].name
                ))
                .show_ui(ui, |ui| {
                    for (index, plot) in self.raw.plots.iter().enumerate() {
                        ui.selectable_value(
                            &mut self.plot,
                            index,
                            format!("{}: {}", index + 1, plot.name),
                        );
                    }
                });
            if self.plot != old_plot {
                self.select_plot();
            }
            let previous = (self.mode, self.log_x);
            egui::ComboBox::from_id_salt("raw-mode")
                .selected_text(self.mode.label())
                .show_ui(ui, |ui| {
                    for mode in Mode::ALL {
                        ui.selectable_value(&mut self.mode, mode, mode.label());
                    }
                });
            ui.add_enabled(
                self.raw.plots[self.plot].scale().is_some(),
                egui::Checkbox::new(&mut self.log_x, "Log X"),
            );
            if previous != (self.mode, self.log_x) {
                self.dirty = true;
                self.fit = true;
            }
            if ui.button("Fit").clicked() {
                self.fit = true;
            }
            ui.weak(format!(
                "{} points · {} signals",
                self.raw.plots[self.plot].points,
                self.selected.len() - usize::from(self.raw.plots[self.plot].scale().is_some())
            ));
            if self.preparing.is_some() {
                ui.spinner();
                ui.label("Preparing traces…");
            }
        });
        ui.weak(&self.raw.plots[self.plot].title);
        ui.weak("Wheel to zoom X · right-drag/middle-drag to pan · Fit resets · hover for interpolated values");
        if self.omitted > 0 {
            ui.weak(format!(
                "{} non-finite/log-ineligible samples omitted (shown as gaps)",
                self.omitted
            ));
        }
        ui.separator();
        let height = ui.available_height().max(180.);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(205., height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.heading("Signals");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("Filter signals")
                            .desired_width(190.),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("All").clicked() {
                            let scale = self.raw.plots[self.plot].scale();
                            for (i, selected) in self.selected.iter_mut().enumerate() {
                                *selected = Some(i) != scale;
                            }
                            self.dirty = true;
                            self.fit = true;
                        }
                        if ui.button("None").clicked() {
                            self.selected.fill(false);
                            self.dirty = true;
                            self.fit = true;
                        }
                    });
                    let plot = &self.raw.plots[self.plot];
                    if self.last_filter != self.filter {
                        let filter = self.filter.to_ascii_lowercase();
                        let scale = plot.scale();
                        self.filtered = plot
                            .variables
                            .iter()
                            .enumerate()
                            .filter(|(i, v)| {
                                Some(*i) != scale && v.name.to_ascii_lowercase().contains(&filter)
                            })
                            .map(|(i, _)| i)
                            .collect();
                        self.last_filter.clone_from(&self.filter);
                    }
                    let row_height = ui.text_style_height(&egui::TextStyle::Body)
                        + if plot.points == 1 { 20. } else { 4. };
                    egui::ScrollArea::vertical()
                        .max_height((height - 95.).max(80.))
                        .show_rows(ui, row_height, self.filtered.len(), |ui, rows| {
                            for row in rows {
                                let index = self.filtered[row];
                                let variable = &plot.variables[index];
                                let label = format!("{} [{}]", variable.name, unit(&variable.kind));
                                if ui.checkbox(&mut self.selected[index], label).changed() {
                                    self.dirty = true;
                                    self.fit = true;
                                }
                                if plot.points == 1 {
                                    ui.monospace(format!(
                                        "  {:.6e}",
                                        self.mode.value(variable.values[0])
                                    ));
                                }
                            }
                        });
                },
            );
            ui.separator();
            self.prepare_traces(ui.ctx());
            self.graph_ui(ui, height);
        });
    }

    fn graph_ui(&mut self, ui: &mut egui::Ui, height: f32) {
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("Layout:");
                ui.selectable_value(&mut self.group_mode, GroupMode::Overlay, "Overlay");
                ui.selectable_value(&mut self.group_mode, GroupMode::ByUnit, "Group by units");
                if ui.button("Separate waves").clicked() {
                    self.group_mode = GroupMode::Separate;
                }
            });
            let data = &self.raw.plots[self.plot];
            let mut full_bounds = PlotBounds::NOTHING;
            self.omitted = 0;
            for (i, trace) in &self.traces {
                if self.selected[*i] {
                    full_bounds.merge(&trace.series.bounds);
                    self.omitted += trace.omitted;
                }
            }
            if !full_bounds.is_finite() {
                full_bounds = PlotBounds::from_min_max([0., -1.], [1., 1.]);
            }
            if !full_bounds.is_valid_x() {
                let x = full_bounds.min()[0];
                full_bounds.extend_with_x(x - 1.);
                full_bounds.extend_with_x(x + 1.);
            }
            if self.fit || self.x_range.is_none() {
                self.x_range = Some((full_bounds.min()[0], full_bounds.max()[0]));
                self.fit = false;
            }

            let timeline_h = 34.;
            let graph_h = (height - timeline_h - 46.).max(120.);
            let width = ui.available_width().max(140.);
            let full_x = (full_bounds.min()[0], full_bounds.max()[0]);
            let (timeline, timeline_response) =
                ui.allocate_exact_size(Vec2::new(width, timeline_h), Sense::click_and_drag());
            ui.add_space(4.);
            let (rect, response) = ui.allocate_exact_size(Vec2::new(width, graph_h), Sense::click_and_drag());

            if let Some((mut min_x, mut max_x)) = self.x_range {
                if response.hovered() || timeline_response.hovered() {
                    let scroll = ui.ctx().input_mut(|input| {
                        let delta = input.smooth_scroll_delta.y;
                        input.smooth_scroll_delta.y = 0.;
                        delta
                    });
                    if scroll != 0. {
                        let pointer = ui.ctx().pointer_hover_pos().unwrap_or(rect.center());
                        let zoom_rect = if timeline_response.hovered() { timeline } else { rect };
                        let anchor = min_x
                            + ((pointer.x - zoom_rect.left()) / zoom_rect.width()).clamp(0., 1.)
                                as f64
                                * (max_x - min_x);
                        let factor = (scroll as f64 * 0.005).clamp(-2., 2.).exp();
                        min_x = anchor - (anchor - min_x) / factor;
                        max_x = anchor + (max_x - anchor) / factor;
                    }
                }
                if response.dragged_by(egui::PointerButton::Secondary)
                    || response.dragged_by(egui::PointerButton::Middle)
                {
                    let pointer_dx = ui.ctx().input(|input| input.pointer.delta().x);
                    let dx = pointer_dx as f64 / rect.width() as f64 * (max_x - min_x);
                    (min_x, max_x) = clamp_x_range(min_x - dx, max_x - dx, full_x);
                    self.x_range = Some((min_x, max_x));
                    ui.ctx().request_repaint();
                } else if timeline_response.dragged() {
                    let pointer_dx = ui.ctx().input(|input| input.pointer.delta().x);
                    let dx = pointer_dx as f64 / timeline.width() as f64 * (full_x.1 - full_x.0);
                    (min_x, max_x) = clamp_x_range(min_x + dx, max_x + dx, full_x);
                    self.x_range = Some((min_x, max_x));
                    ui.ctx().request_repaint();
                } else {
                    self.x_range = Some(clamp_x_range(min_x, max_x, full_x));
                }
            }
            let (min_x, max_x) = self.x_range.unwrap();
            let padding = (max_x - min_x) * 0.2;
            let selected_count = self.selected.iter().filter(|&&selected| selected).count().max(1);
            let base_columns = rect.width() * ui.ctx().pixels_per_point();
            // epaint tessellation cost is roughly proportional to total polyline
            // vertices, so keep the frame budget approximately constant when
            // hundreds of traces are visible. The extrema LOD emits up to four
            // vertices per column per trace, so divide the global budget by 4N.
            let point_budget = 25_000. / selected_count as f32 / 4.;
            let columns = if selected_count > 24 {
                base_columns.min(point_budget).clamp(8., 4096.)
            } else {
                base_columns.clamp(64., 4096.)
            };
            let view = View {
                min: min_x - padding,
                max: max_x + padding,
                columns: columns as usize,
            };
            for (i, trace) in &mut self.traces {
                if self.selected[*i] {
                    trace.series.prepare(view);
                }
            }

            let groups = self.trace_groups();
            let painter = ui.painter();
            painter.rect_filled(rect, 0., Color32::from_rgb(18, 20, 24));
            painter.rect_stroke(rect, 0., Stroke::new(1., Color32::DARK_GRAY), egui::StrokeKind::Inside);
            painter.rect_filled(timeline, 0., Color32::from_rgb(14, 16, 19));
            painter.rect_stroke(timeline, 0., Stroke::new(1., Color32::DARK_GRAY), egui::StrokeKind::Inside);
            let plot_area = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.bottom() - 26.));
            let row_h = plot_area.height() / groups.len().max(1) as f32;
            for (row, group) in groups.iter().enumerate() {
                let row_rect = Rect::from_min_max(
                    Pos2::new(plot_area.left(), plot_area.top() + row as f32 * row_h),
                    Pos2::new(plot_area.right(), plot_area.top() + (row + 1) as f32 * row_h),
                ).shrink2(Vec2::new(64., 10.));
                let y_bounds = self.group_bounds(group);
                if row_rect.height() >= 8. {
                    draw_grid(painter, row_rect, (min_x, max_x), y_bounds);
                    painter.line_segment([Pos2::new(row_rect.left(), row_rect.center().y), Pos2::new(row_rect.right(), row_rect.center().y)], Stroke::new(1., Color32::from_gray(50)));
                }
                let unit = group.iter().map(|i| self.trace_unit(*i)).next().unwrap_or("");
                if row_rect.height() >= 24. {
                    draw_y_axis(painter, row_rect, y_bounds, unit);
                }
                let row_clip = painter.with_clip_rect(row_rect);
                for &index in group {
                    let trace = &self.traces[&index];
                    draw_trace(&row_clip, trace, row_rect, (min_x, max_x), y_bounds, true);
                }
            }
            let tclip = painter.with_clip_rect(timeline);
            let all: Vec<_> = groups.iter().flat_map(|g| g.iter().copied()).collect();
            let y_bounds = self.group_bounds(&all);
            let timeline_view = View {
                min: full_x.0,
                max: full_x.1,
                // The overview is cached, but still has to be tessellated every
                // frame. Keep a total vertex budget when many traces overlap.
                columns: (timeline.width() * ui.ctx().pixels_per_point())
                    .min(6_000. / selected_count as f32 / 4.)
                    .clamp(8., 8192.) as usize,
            };
            for &index in &all {
                let trace = self.traces.get_mut(&index).unwrap();
                if trace.overview_view != Some(timeline_view) {
                    trace.overview = trace.series.sampled(timeline_view);
                    trace.overview_view = Some(timeline_view);
                }
            }
            for &index in &all {
                let trace = &self.traces[&index];
                draw_sampled_trace(
                    &tclip,
                    trace,
                    &trace.overview,
                    timeline.shrink2(Vec2::new(4., 4.)),
                    full_x,
                    y_bounds,
                );
            }
            let sel_l = value_to_screen(min_x, full_x, timeline.left(), timeline.right());
            let sel_r = value_to_screen(max_x, full_x, timeline.left(), timeline.right());
            painter.rect_stroke(Rect::from_min_max(Pos2::new(sel_l, timeline.top()), Pos2::new(sel_r, timeline.bottom())), 0., Stroke::new(1., Color32::WHITE), egui::StrokeKind::Inside);
            let x_unit = data.scale().map(|i| unit(&data.variables[i].kind)).unwrap_or("");
            draw_x_axis(painter, plot_area, (min_x, max_x), x_unit, self.log_x);

            if response.hovered()
                && let Some(pointer) = response.hover_pos()
            {
                let x_left = plot_area.left() + 64.;
                let x_right = plot_area.right() - 64.;
                let x = min_x
                    + ((pointer.x - x_left) / (x_right - x_left)).clamp(0., 1.) as f64
                        * (max_x - min_x);
                let cursor = PlotPoint::new(x, 0.);
                if let Some((trace, point, screen)) = self.closest_trace(&groups, plot_area, pointer, cursor, (min_x, max_x)) {
                    let y_unit = self.trace_unit(trace.index);
                    painter.circle_filled(screen, 4.5, Color32::BLACK);
                    painter.circle_filled(screen, 3.2, trace_color(trace.index));
                    painter.circle_stroke(screen, 4.5, Stroke::new(1., Color32::WHITE));
                    egui::Area::new(egui::Id::new("waveform-marker-readout"))
                        .order(egui::Order::Foreground)
                        .fixed_pos(screen + Vec2::new(10., 10.))
                        .interactable(false)
                        .show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style()).show(ui, |ui| {
                                ui.set_min_width(220.);
                                ui.add(egui::Label::new(&trace.label).extend());
                                ui.add(
                                    egui::Label::new(egui::RichText::new(format!(
                                        "x = {}\ny = {}",
                                        si_value(
                                            if self.log_x { 10_f64.powf(point.x) } else { point.x },
                                            x_unit,
                                        ),
                                        si_value(point.y, y_unit)
                                    )).monospace())
                                    .extend(),
                                );
                            });
                        });
                }
            }
        });
    }

    fn trace_groups(&self) -> Vec<Vec<usize>> {
        let selected: Vec<_> = self.traces.keys().copied().filter(|i| self.selected[*i]).collect();
        match self.group_mode {
            GroupMode::Overlay => vec![selected],
            GroupMode::Separate => selected.into_iter().map(|i| vec![i]).collect(),
            GroupMode::ByUnit => {
                let mut map: BTreeMap<String, Vec<usize>> = BTreeMap::new();
                for i in selected {
                    map.entry(self.trace_unit(i).to_owned()).or_default().push(i);
                }
                map.into_values().collect()
            }
        }.into_iter().filter(|g| !g.is_empty()).collect()
    }
    fn trace_unit(&self, index: usize) -> &str {
        let variable = &self.raw.plots[self.plot].variables[index];
        match self.mode {
            Mode::Phase => "deg",
            Mode::Decibels => "dB",
            _ => unit(&variable.kind),
        }
    }
    fn group_bounds(&self, group: &[usize]) -> (f64, f64) {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for &i in group {
            let b = self.traces[&i].series.bounds;
            min = min.min(b.min()[1]);
            max = max.max(b.max()[1]);
        }
        if !min.is_finite() || !max.is_finite() { (-1., 1.) } else if min == max { (min - 1., max + 1.) } else { (min, max) }
    }
    fn closest_trace<'a>(&'a self, groups: &[Vec<usize>], rect: Rect, pointer: Pos2, cursor: PlotPoint, x_bounds: (f64, f64)) -> Option<(&'a Trace, PlotPoint, Pos2)> {
        let row_h = rect.height() / groups.len().max(1) as f32;
        groups.iter().enumerate().flat_map(|(row, group)| {
            let row_rect = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + row as f32 * row_h), Pos2::new(rect.right(), rect.top() + (row + 1) as f32 * row_h)).shrink2(Vec2::new(64., 10.));
            let y_bounds = self.group_bounds(group);
            group.iter().filter_map(move |i| {
                let trace = &self.traces[i];
                let point = trace.series.interpolate(cursor)?;
                let screen = point_to_screen(point, row_rect, x_bounds, y_bounds);
                Some((trace, point, screen, (screen.y - pointer.y).abs()))
            })
        }).min_by(|a, b| a.3.total_cmp(&b.3)).filter(|c| c.3 <= 16.).map(|(a,b,c,_)| (a,b,c))
    }

    fn prepare_traces(&mut self, ctx: &egui::Context) {
        let projection = (self.plot, self.mode, self.log_x);
        if self.projection != projection {
            self.projection = projection;
            self.traces.clear();
            self.preparing = None; // Discard stale projection results.
            self.dirty = true;
        }
        if let Some(traces) = self
            .preparing
            .as_ref()
            .and_then(|job| job.receiver.try_recv().ok())
        {
            self.preparing = None;
            self.traces.extend(traces.into_iter().map(|t| (t.index, t)));
            self.fit = true;
            self.dirty = true;
        }
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if self.preparing.is_some() {
            return;
        }
        let missing: Vec<_> = self
            .selected
            .iter()
            .enumerate()
            .filter(|(i, enabled)| **enabled && !self.traces.contains_key(i))
            .map(|(i, _)| i)
            .collect();
        if missing.is_empty() {
            return;
        }
        let raw = Arc::clone(&self.raw);
        let (tx, rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        self.preparing = Some(Preparation {
            receiver: rx,
            cancelled: Arc::clone(&cancelled),
        });
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let (plot, mode, log_x) = projection;
            let mut traces = Vec::new();
            for i in missing {
                if cancelled.load(Ordering::Relaxed) {
                    return;
                }
                traces.push(Trace::new(&raw.plots[plot], i, mode, log_x));
            }
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            let _ = tx.send(traces);
            ctx.request_repaint_of(viewport_id());
            if ctx.embed_viewports() {
                ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
        });
    }
}

fn trace_points(
    plot: &RawPlot,
    index: usize,
    mode: Mode,
    log_x: bool,
) -> (Vec<Vec<PlotPoint>>, usize) {
    let mut segments = Vec::new();
    let mut segment = Vec::new();
    let mut omitted = 0;
    let scale = plot.scale();
    for (point, value) in plot.variables[index].values.iter().enumerate() {
        let x = scale
            .map(|i| plot.variables[i].values[point].real)
            .unwrap_or(point as f64);
        let x = if log_x { x.log10() } else { x };
        let y = mode.value(*value);
        if x.is_finite() && y.is_finite() {
            segment.push(PlotPoint::new(x, y));
        } else {
            omitted += 1;
            if !segment.is_empty() {
                segments.push(std::mem::take(&mut segment));
            }
        }
    }
    if !segment.is_empty() {
        segments.push(segment);
    }
    (segments, omitted)
}

fn si_value(value: f64, unit: &str) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    if value == 0. {
        return format!("0{unit}");
    }
    let abs = value.abs();
    let prefixes = [
        (1e12, "T"),
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1., ""),
        (1e-3, "m"),
        (1e-6, "u"),
        (1e-9, "n"),
        (1e-12, "p"),
        (1e-15, "f"),
    ];
    let (scale, prefix) = prefixes
        .into_iter()
        .find(|(scale, _)| abs >= *scale)
        .unwrap_or((1e-15, "f"));
    format!("{}{}{}", scaled_number(value, scale), prefix, unit)
}

fn axis_prefix(max_abs: f64) -> (f64, &'static str) {
    if !max_abs.is_finite() || max_abs == 0. {
        return (1., "");
    }
    for (scale, prefix) in [
        (1e12, "T"),
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1., ""),
        (1e-3, "m"),
        (1e-6, "u"),
        (1e-9, "n"),
        (1e-12, "p"),
        (1e-15, "f"),
    ] {
        if max_abs >= scale {
            return (scale, prefix);
        }
    }
    (1e-15, "f")
}

fn scaled_number(value: f64, scale: f64) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    format!("{:.3}", value / scale)
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn fixed_scaled_number(value: f64, scale: f64, precision: usize) -> String {
    if !value.is_finite() {
        return "—".into();
    }
    format!("{:.*}", precision, value / scale)
}

fn ruler_precision(values: &[f64], scale: f64) -> usize {
    let mut precision = 0;
    while precision < 12 {
        let labels: Vec<_> = values
            .iter()
            .map(|&value| fixed_scaled_number(value, scale, precision))
            .collect();
        if labels.windows(2).all(|pair| pair[0] != pair[1]) {
            return precision;
        }
        precision += 1;
    }
    precision
}

fn draw_y_axis(painter: &egui::Painter, row: Rect, bounds: (f64, f64), unit: &str) {
    let font = egui::FontId::monospace(10.);
    let color = Color32::GRAY;
    let (scale, prefix) = axis_prefix(bounds.0.abs().max(bounds.1.abs()));
    let axis_unit = format!("{prefix}{unit}");
    let axis_x = row.left();
    painter.line_segment(
        [Pos2::new(axis_x, row.top()), Pos2::new(axis_x, row.bottom())],
        Stroke::new(1., Color32::from_gray(95)),
    );
    let step = nice_step(bounds.1 - bounds.0, (row.height() / 34.).max(2.) as f64);
    let first = (bounds.0 / step).ceil() as i64;
    let last = (bounds.1 / step).floor() as i64;
    for tick in first - 1..=last {
        for sub in 1..=10 {
            let value = (tick as f64 + sub as f64 / 11.) * step;
            if value <= bounds.0 || value >= bounds.1 {
                continue;
            }
            let y = value_to_screen(value, bounds, row.bottom(), row.top());
            painter.line_segment(
                [Pos2::new(axis_x - 2., y), Pos2::new(axis_x, y)],
                Stroke::new(1., Color32::from_gray(70)),
            );
        }
    }
    let ticks: Vec<_> = (first..=last).map(|tick| tick as f64 * step).collect();
    let precision = ruler_precision(&ticks, scale);
    for value in ticks {
        let y = value_to_screen(value, bounds, row.bottom(), row.top());
        painter.line_segment(
            [Pos2::new(axis_x - 4., y), Pos2::new(axis_x, y)],
            Stroke::new(1., Color32::from_gray(95)),
        );
        painter.text(
            Pos2::new(axis_x - 7., y),
            egui::Align2::RIGHT_CENTER,
            format!("{}{}", fixed_scaled_number(value, scale, precision), axis_unit),
            font.clone(),
            color,
        );
    }
}

fn nice_step(span: f64, target_lines: f64) -> f64 {
    let raw = span.abs() / target_lines.max(1.);
    if !raw.is_finite() || raw <= 0. {
        return 1.;
    }
    let decade = 10_f64.powf(raw.log10().floor());
    for multiple in [1., 2., 5., 10.] {
        let step = multiple * decade;
        if raw <= step {
            return step;
        }
    }
    decade * 10.
}

fn draw_x_axis(
    painter: &egui::Painter,
    plot_area: Rect,
    bounds: (f64, f64),
    unit: &str,
    log: bool,
) {
    let font = egui::FontId::monospace(10.);
    let color = Color32::GRAY;
    let y = plot_area.bottom();
    let left = plot_area.left() + 64.;
    let right = plot_area.right() - 64.;
    let max_abs = if log {
        10_f64.powf(bounds.0).abs().max(10_f64.powf(bounds.1).abs())
    } else {
        bounds.0.abs().max(bounds.1.abs())
    };
    let (scale, prefix) = axis_prefix(max_abs);
    let axis_unit = format!("{prefix}{unit}");
    painter.line_segment(
        [Pos2::new(left, y), Pos2::new(right, y)],
        Stroke::new(1., Color32::from_gray(95)),
    );
    let step = nice_step(bounds.1 - bounds.0, ((right - left) / 85.).max(2.) as f64);
    let first = (bounds.0 / step).ceil() as i64;
    let last = (bounds.1 / step).floor() as i64;
    for tick in first - 1..=last {
        for sub in 1..=10 {
            let value = (tick as f64 + sub as f64 / 11.) * step;
            if value <= bounds.0 || value >= bounds.1 {
                continue;
            }
            let x = value_to_screen(value, bounds, left, right);
            painter.line_segment(
                [Pos2::new(x, y), Pos2::new(x, y + 2.)],
                Stroke::new(1., Color32::from_gray(70)),
            );
        }
    }
    let ticks: Vec<_> = (first..=last).map(|tick| tick as f64 * step).collect();
    let display_values: Vec<_> = ticks
        .iter()
        .map(|&value| if log { 10_f64.powf(value) } else { value })
        .collect();
    let precision = ruler_precision(&display_values, scale);
    for (value, display_value) in ticks.into_iter().zip(display_values) {
        let x = value_to_screen(value, bounds, left, right);
        painter.line_segment(
            [Pos2::new(x, y), Pos2::new(x, y + 4.)],
            Stroke::new(1., Color32::from_gray(95)),
        );
        painter.text(
            Pos2::new(x, y + 6.),
            egui::Align2::CENTER_TOP,
            format!("{}{}", fixed_scaled_number(display_value, scale, precision), axis_unit),
            font.clone(),
            color,
        );
    }
}

fn draw_grid(painter: &egui::Painter, rect: Rect, x: (f64, f64), y: (f64, f64)) {
    let stroke = Stroke::new(1., Color32::from_gray(34));
    let major_stroke = Stroke::new(1., Color32::from_gray(42));

    let x_step = nice_step(x.1 - x.0, (rect.width() / 90.).max(2.) as f64);
    let first_x = (x.0 / x_step).ceil() as i64;
    let last_x = (x.1 / x_step).floor() as i64;
    for tick in first_x..=last_x {
        let value = tick as f64 * x_step;
        let sx = value_to_screen(value, x, rect.left(), rect.right());
        painter.line_segment(
            [Pos2::new(sx, rect.top()), Pos2::new(sx, rect.bottom())],
            if tick % 5 == 0 { major_stroke } else { stroke },
        );
    }

    let y_step = nice_step(y.1 - y.0, (rect.height() / 45.).max(2.) as f64);
    let first_y = (y.0 / y_step).ceil() as i64;
    let last_y = (y.1 / y_step).floor() as i64;
    for tick in first_y..=last_y {
        let value = tick as f64 * y_step;
        let sy = value_to_screen(value, y, rect.bottom(), rect.top());
        painter.line_segment(
            [Pos2::new(rect.left(), sy), Pos2::new(rect.right(), sy)],
            if tick % 5 == 0 { major_stroke } else { stroke },
        );
    }
}

fn clamp_x_range(mut min: f64, mut max: f64, full: (f64, f64)) -> (f64, f64) {
    let full_width = full.1 - full.0;
    let mut width = max - min;
    if !width.is_finite() || width <= 0. || width >= full_width {
        return full;
    }
    if min < full.0 {
        max += full.0 - min;
        min = full.0;
    }
    if max > full.1 {
        min -= max - full.1;
        max = full.1;
    }
    if min < full.0 || max > full.1 {
        width = width.min(full_width);
        let center = ((min + max) * 0.5).clamp(full.0 + width * 0.5, full.1 - width * 0.5);
        min = center - width * 0.5;
        max = center + width * 0.5;
    }
    (min, max)
}

fn value_to_screen(value: f64, bounds: (f64, f64), min: f32, max: f32) -> f32 {
    let denom = bounds.1 - bounds.0;
    if denom == 0. || !denom.is_finite() {
        return (min + max) * 0.5;
    }
    min + ((value - bounds.0) / denom) as f32 * (max - min)
}
fn point_to_screen(point: PlotPoint, rect: Rect, x: (f64, f64), y: (f64, f64)) -> Pos2 {
    Pos2::new(
        value_to_screen(point.x, x, rect.left(), rect.right()),
        value_to_screen(point.y, y, rect.bottom(), rect.top()),
    )
}
fn draw_sampled_trace(
    painter: &egui::Painter,
    trace: &Trace,
    segments: &[Vec<PlotPoint>],
    rect: Rect,
    x: (f64, f64),
    y: (f64, f64),
) {
    let color = trace_color(trace.index);
    for segment in segments {
        if segment.is_empty() {
            continue;
        }
        let points: Vec<_> = segment.iter().map(|p| point_to_screen(*p, rect, x, y)).collect();
        if points.len() >= 2 {
            painter.add(egui::Shape::line(points, Stroke::new(1., color.gamma_multiply(0.75))));
        }
    }
}

fn draw_trace(
    painter: &egui::Painter,
    trace: &Trace,
    rect: Rect,
    x: (f64, f64),
    y: (f64, f64),
    dots: bool,
) {
    let color = trace_color(trace.index);
    for segment in &trace.series.rendered {
        if segment.is_empty() {
            continue;
        }
        let points: Vec<_> = segment.iter().map(|p| point_to_screen(*p, rect, x, y)).collect();
        if points.len() >= 2 {
            painter.add(egui::Shape::line(points.clone(), Stroke::new(1.4, color)));
        }
        if dots && trace.series.rendered_full_resolution {
            for p in points {
                painter.circle_filled(p, 2., color);
            }
        }
    }
}

fn unit(kind: &str) -> &str {
    match kind {
        "voltage" => "V",
        "current" => "A",
        "time" => "s",
        "frequency" => "Hz",
        _ => kind,
    }
}
fn trace_color(index: usize) -> Color32 {
    const COLORS: [Color32; 6] = [
        Color32::LIGHT_GREEN,
        Color32::LIGHT_BLUE,
        Color32::LIGHT_RED,
        Color32::YELLOW,
        Color32::KHAKI,
        Color32::LIGHT_GRAY,
    ];
    COLORS[index % COLORS.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_coordinate_mapping_keeps_y_independent_from_x_zoom() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(100., 50.));
        let p = point_to_screen(PlotPoint::new(5., 0.), rect, (0., 10.), (-1., 1.));
        assert_eq!(p.x, 50.);
        assert_eq!(p.y, 25.);
    }
    #[test]
    fn complex_projection_and_invalid_samples_make_gaps() {
        let sample = Sample {
            real: 3.,
            imaginary: -4.,
        };
        assert_eq!(Mode::Magnitude.value(sample), 5.);
        assert!((Mode::Decibels.value(sample) - 13.9794000867).abs() < 1e-9);
        assert!((Mode::Phase.value(sample) + 53.1301023542).abs() < 1e-9);
        let raw = RawFile::parse(b"Title: test\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 2\nNo. Points: 3\nVariables:\n0 time time\n1 v(out) voltage\nValues:\n0 1\n2\n1 2\nnan\n2 3\n4\n").unwrap();
        let (segments, omitted) = trace_points(&raw.plots[0], 1, Mode::Real, false);
        assert_eq!(omitted, 1);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0][0], PlotPoint::new(1., 2.));
        assert_eq!(segments[1][0], PlotPoint::new(3., 4.));
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
        assert_eq!(
            waveforms
                .state
                .lock()
                .unwrap()
                .data
                .as_ref()
                .unwrap()
                .traces[&0]
                .series
                .rendered[0][0],
            PlotPoint::new(0., 2.)
        );
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

    #[test]
    fn trace_preparation_is_async_cached_and_discards_old_projections() {
        let raw = RawFile::parse(b"Title: divider\nPlotname: Operating Point\nFlags: real\nNo. Variables: 2\nNo. Points: 1\nVariables:\n0 v(out) voltage\n1 i(v1) current\nValues:\n0 2\n-1e-4\n").unwrap();
        let mut display = Display::new("divider.raw".into(), raw);
        let ctx = egui::Context::default();
        display.selected[1] = true;
        display.dirty = true;
        display.prepare_traces(&ctx);
        assert!(display.preparing.is_some());
        // Change projection while the real-valued job is still in flight.
        display.mode = Mode::Imaginary;
        display.prepare_traces(&ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while display.preparing.is_some() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
            display.prepare_traces(&ctx);
        }
        assert_eq!(display.traces.len(), 2);
        for trace in display.traces.values() {
            assert_eq!(trace.series.bounds.min()[1], 0.);
            assert_eq!(trace.series.bounds.max()[1], 0.);
        }
        display.selected[1] = false;
        display.dirty = true;
        display.prepare_traces(&ctx);
        display.selected[1] = true;
        display.dirty = true;
        display.prepare_traces(&ctx);
        assert!(
            display.preparing.is_none(),
            "re-enabling a cached trace must not reproject it"
        );
        assert_eq!(display.traces.len(), 2);
    }
}
