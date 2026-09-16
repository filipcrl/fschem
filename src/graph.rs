//! Reusable waveform model, projection cache, axes and trace rendering.
use crate::waveform_lod::{PlotBounds, PlotPoint, Series, View};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use fschem::raw::{RawFile, RawPlot, Sample};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
fn signal_label(native: &str, aliases: &BTreeMap<String, String>) -> String {
    let names: Vec<_> = aliases.iter().filter(|(alias, target)| alias.starts_with('/') && target.as_str() == native).map(|(alias, _)| alias.as_str()).collect();
    if names.is_empty() { native.into() } else { format!("{} ({native})", names.join(", ")) }
}
fn artifact_aliases(path: &std::path::Path, raw: &RawFile) -> Vec<BTreeMap<String, String>> {
    let mut aliases = vec![BTreeMap::new(); raw.plots.len()];
    let Some(parent) = path.parent() else { return aliases; };
    if let Ok(run) = crate::run_artifact::RunArtifact::open(&parent.join("run.json")) {
        for result in &run.manifest.results {
            let matches_path = crate::run_artifact::contained(&run.root, &result.path).ok().and_then(|p| p.canonicalize().ok()) == path.canonicalize().ok();
            if matches_path && let Some(plot) = raw.plots.get(result.plot_index) {
                // Stale or mismatched manifests must not relabel another dataset.
                if plot.name == result.name && result.signals == plot.variables.iter().map(|v| v.name.clone()).collect::<Vec<_>>() {
                    aliases[result.plot_index] = result.signal_map.clone();
                }
            }
        }
    }
    aliases
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
    fn new(plot: &RawPlot, index: usize, mode: Mode, log_x: bool, aliases: &BTreeMap<String, String>) -> Self {
        let (segments, omitted) = trace_points(plot, index, mode, log_x);
        let variable = &plot.variables[index];
        let units = match mode {
            Mode::Phase => "degrees",
            Mode::Decibels => "dB",
            _ => variable.unit.as_str(),
        };
        Self {
            index,
            label: format!("{} [{units}]", signal_label(&variable.name, aliases)),
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

pub(crate) struct Display {
    pub(crate) path: PathBuf,
    raw: Arc<RawFile>,
    aliases: Arc<Vec<BTreeMap<String, String>>>,
    plot: usize,
    selected: Vec<bool>,
    filter: String,
    expression: String,
    expression_error: Option<String>,
    calculated: Vec<(String, String)>,
    mode: Mode,
    log_x: bool,
    fit: bool,
    x_range: Option<(f64, f64)>,
    y_range: Option<(f64, f64)>,
    group_mode: GroupMode,
    timeline_dragging: bool,
    dirty: bool,
    traces: BTreeMap<usize, Trace>,
    projection: (usize, Mode, bool),
    preparing: Option<Preparation>,
    filtered: Vec<usize>,
    last_filter: String,
    omitted: usize,
}
impl Display {
    pub(crate) fn new(path: PathBuf, mut raw: RawFile) -> Self {
        let aliases = artifact_aliases(&path, &raw);
        for (plot, aliases) in raw.plots.iter_mut().zip(&aliases) {
            for variable in &mut plot.variables {
                if variable.kind == "notype" && aliases.iter().any(|(canonical,native)| canonical.starts_with('/') && canonical != native && native == &variable.name) {
                    variable.kind = "voltage".into();
                    variable.unit = "V".into();
                }
            }
        }
        let mut result = Self {
            aliases: Arc::new(aliases),
            path,
            raw: Arc::new(raw),
            plot: 0,
            selected: Vec::new(),
            filter: String::new(),
            expression: String::new(),
            expression_error: None,
            calculated: Vec::new(),
            mode: Mode::Real,
            log_x: false,
            fit: true,
            x_range: None,
            y_range: None,
            group_mode: GroupMode::Overlay,
            timeline_dragging: false,
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
                    Trace::new(&result.raw.plots[result.plot], i, result.mode, result.log_x, &result.aliases[result.plot]),
                );
            }
        }
        result.dirty = false;
        result
    }
    fn add_expression(&mut self, expression: &str) -> Result<(), String> {
        let plot = &self.raw.plots[self.plot];
        if plot.variables.iter().any(|v| v.name == expression) {
            return Err("This signal/expression already exists".into());
        }
        let variable = fschem::units::calculate_with_aliases(plot, expression, &self.aliases[self.plot])?;
        self.calculated.push((plot.name.clone(), expression.into()));
        Arc::make_mut(&mut self.raw).plots[self.plot].variables.push(variable);
        self.selected.push(true);
        self.dirty = true;
        self.fit = true;
        self.last_filter = "\0".into();
        Ok(())
    }
    pub(crate) fn open_plot(&mut self, index: usize) -> Result<(), String> {
        if index >= self.raw.plots.len() { return Err("Published plot is missing from raw file".into()); }
        self.plot = index;
        self.select_plot();
        self.traces.clear();
        self.dirty = true;
        Ok(())
    }
    /// Keep signal selection and navigation when the underlying file is replaced.
    pub(crate) fn restore_view(&mut self, previous: &Self) {
        for (name, expression) in &previous.calculated {
            if let Some(index) = self.raw.plots.iter().position(|p| &p.name == name) {
                self.plot = index;
                self.select_plot();
                if let Err(error) = self.add_expression(expression) { self.expression_error = Some(error); }
            }
        }
        let old = &previous.raw.plots[previous.plot];
        self.plot = self
            .raw
            .plots
            .iter()
            .position(|p| p.name == old.name)
            .unwrap_or(0);
        self.select_plot();
        self.mode = previous.mode;
        self.log_x = previous.log_x;
        self.group_mode = previous.group_mode;
        self.x_range = previous.x_range;
        self.y_range = previous.y_range;
        self.fit = previous.fit;
        self.filter = previous.filter.clone();
        self.selected = self.raw.plots[self.plot]
            .variables
            .iter()
            .map(|v| {
                old.variables
                    .iter()
                    .enumerate()
                    .any(|(i, o)| previous.selected[i] && o.name == v.name)
            })
            .collect();
        self.traces.clear();
        self.dirty = true;
    }

    /// Configure a schematic graph using xschem's graph properties.
    pub(crate) fn configure(&mut self, props: &fschem::document::Properties) {
        self.plot = props.get("dataset").and_then(|s| s.parse::<usize>().ok()).unwrap_or(0).min(self.raw.plots.len() - 1);
        self.select_plot();
        if let Some(nodes) = props.get("node") {
            let names: Vec<_> = nodes
                .split_whitespace()
                .map(|s| s.rsplit(';').next().unwrap_or(s))
                .collect();
            let plot = &self.raw.plots[self.plot];
            self.selected = plot
                .variables
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    Some(i) != plot.scale()
                        && names.iter().any(|n| {
                            self.aliases[self.plot].get(*n).is_some_and(|native| native == &v.name)
                                || v.name.eq_ignore_ascii_case(n)
                                || v.name.eq_ignore_ascii_case(&format!("v({n})"))
                        })
                })
                .collect();
        }
        if let Some(log) = props.get("logx") {
            self.log_x = log == "1";
        }
        self.group_mode = match props.get("groupmode") {
            Some("separate") => GroupMode::Separate,
            Some("unit") => GroupMode::ByUnit,
            _ => GroupMode::Overlay,
        };
        self.mode = match props.get("mode") {
            Some("imaginary") => Mode::Imaginary,
            Some("magnitude") => Mode::Magnitude,
            Some("decibels") => Mode::Decibels,
            Some("phase") => Mode::Phase,
            Some("real") => Mode::Real,
            _ => self.mode,
        };
        self.x_range = props.get("x1").and_then(|s| s.parse::<f64>().ok())
            .zip(props.get("x2").and_then(|s| s.parse::<f64>().ok()))
            .filter(|(a,b)| a.is_finite() && b.is_finite() && a < b);
        self.y_range = props.get("y1").and_then(|s| s.parse::<f64>().ok())
            .zip(props.get("y2").and_then(|s| s.parse::<f64>().ok()))
            .filter(|(a,b)| a.is_finite() && b.is_finite() && a < b);
        self.fit = self.x_range.is_none();
        self.dirty = true;
    }

    pub(crate) fn x_range(&self) -> Option<(f64, f64)> { self.x_range }

    /// Only properties owned by waveform controls; unrelated graph fields survive edits.
    pub(crate) fn configuration(&self) -> BTreeMap<String, String> {
        let nodes = self.raw.plots[self.plot].variables.iter().zip(&self.selected)
            .filter(|(_, selected)| **selected).map(|(v, _)| v.name.as_str()).collect::<Vec<_>>().join(" ");
        BTreeMap::from([
            ("dataset".into(), self.plot.to_string()),
            ("node".into(), nodes),
            ("logx".into(), if self.log_x { "1" } else { "0" }.into()),
            ("groupmode".into(), match self.group_mode { GroupMode::Overlay => "overlay", GroupMode::ByUnit => "unit", GroupMode::Separate => "separate" }.into()),
            ("mode".into(), match self.mode { Mode::Real => "real", Mode::Imaginary => "imaginary", Mode::Magnitude => "magnitude", Mode::Decibels => "decibels", Mode::Phase => "phase" }.into()),
        ])
    }

    /// Paint without allocating interactive widgets: schematic editing owns gestures.
    /// Uses the same projection, extrema LOD, grid, axes and trace painter as the window.
    pub(crate) fn paint_embedded(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        props: &fschem::document::Properties,
    ) {
        self.prepare_traces(ui.ctx());
        if self.group_mode != GroupMode::Overlay && !self.trace_groups().is_empty() {
            let selected = self.selected.clone();
            let mode = self.group_mode;
            let groups = self.trace_groups();
            self.group_mode = GroupMode::Overlay;
            for (row, group) in groups.iter().enumerate() {
                self.selected.fill(false);
                for &index in group { self.selected[index] = true; }
                let height = rect.height() / groups.len() as f32;
                let panel = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + row as f32 * height), Pos2::new(rect.right(), rect.top() + (row + 1) as f32 * height));
                self.paint_embedded(ui, panel, props);
            }
            self.selected = selected;
            self.group_mode = mode;
            return;
        }
        self.prepare_traces(ui.ctx());
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        painter.rect_filled(rect, 0., Color32::from_rgb(18, 20, 24));
        if rect.width() < 150. || rect.height() < 40. {
            return;
        }
        let area = Rect::from_min_max(
            rect.min + Vec2::new(60., if rect.height() < 85. { 14. } else { 20. }),
            rect.max - Vec2::new(16., if rect.height() < 85. { 20. } else { 28. }),
        );
        let mut bounds = PlotBounds::NOTHING;
        for (i, trace) in &self.traces {
            if self.selected[*i] {
                bounds.merge(&trace.series.bounds);
            }
        }
        // Xschem stores logarithmic graph limits in log10 coordinates already.
        let range = |a: &str, b: &str, default: (f64, f64)| {
            let parse = |key| props.get(key).and_then(|s| s.parse::<f64>().ok());
            let pair = (parse(a).unwrap_or(default.0), parse(b).unwrap_or(default.1));
            if pair.0.is_finite() && pair.1.is_finite() && pair.0 < pair.1 {
                pair
            } else {
                default
            }
        };
        let fallback = |a: f64, b: f64| {
            if a.is_finite() && b.is_finite() {
                if a < b { (a, b) } else { (a - 1., b + 1.) }
            } else {
                (0., 1.)
            }
        };
        let x = range("x1", "x2", fallback(bounds.min()[0], bounds.max()[0]));
        let y = range("y1", "y2", fallback(bounds.min()[1], bounds.max()[1]));
        draw_grid(&painter, area, x, y);
        draw_y_axis(&painter, area, y, "");
        let plot = &self.raw.plots[self.plot];
        let x_unit = plot
            .scale()
            .map(|i| plot.variables[i].unit.as_str())
            .unwrap_or("");
        draw_x_axis(&painter, area, x, x_unit, self.log_x);
        let clip = painter.with_clip_rect(area.intersect(painter.clip_rect()));
        let count = self.selected.iter().filter(|s| **s).count().max(1);
        let columns = (area.width() * ui.ctx().pixels_per_point())
            .min(6250. / count as f32)
            .clamp(8., 4096.) as usize;
        let mut labels = Vec::new();
        for (i, trace) in &mut self.traces {
            if self.selected[*i] {
                trace.series.prepare(View {
                    min: x.0,
                    max: x.1,
                    columns,
                });
                draw_trace(&clip, trace, area, x, y, true);
                labels.push(trace.label.clone());
            }
        }
        painter.text(
            rect.left_top() + Vec2::new(6., 3.),
            egui::Align2::LEFT_TOP,
            if labels.is_empty() {
                "No matching signals".into()
            } else {
                labels.join("   ")
            },
            egui::FontId::proportional(11.),
            Color32::LIGHT_GRAY,
        );
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
        self.y_range = None;
    }
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui) {
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
        ui.horizontal(|ui| {
            ui.label("Expression:");
            let response = ui.add(egui::TextEdit::singleline(&mut self.expression)
                .hint_text("v(OUT)/v(IN), v(OUT)/i(VDD), deriv(v(OUT))")
                .desired_width(360.));
            if ui.button("Add trace").clicked() || (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                let expression = self.expression.trim().to_owned();
                self.expression_error = self.add_expression(&expression).err();
            }
        });
        if let Some(error) = &self.expression_error { ui.colored_label(Color32::LIGHT_RED, error); }
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
                                Some(*i) != scale && signal_label(&v.name, &self.aliases[self.plot]).to_ascii_lowercase().contains(&filter)
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
                                let label = format!("{} [{}]", signal_label(&variable.name, &self.aliases[self.plot]), variable.unit);
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
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(width, graph_h), Sense::click_and_drag());

            if let Some((mut min_x, mut max_x)) = self.x_range {
                if response.hovered() || timeline_response.hovered() {
                    let scroll = ui.ctx().input_mut(|input| {
                        let delta = input.smooth_scroll_delta.y;
                        input.smooth_scroll_delta.y = 0.;
                        delta
                    });
                    if scroll != 0. {
                        let pointer = ui.ctx().pointer_hover_pos().unwrap_or(rect.center());
                        let zoom_rect = if timeline_response.hovered() {
                            timeline
                        } else {
                            rect
                        };
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
                } else if timeline_response.drag_started()
                    && let Some(pointer) = timeline_response.interact_pointer_pos()
                {
                    self.timeline_dragging = true;
                    let current_left =
                        value_to_screen(min_x, full_x, timeline.left(), timeline.right());
                    let current_right =
                        value_to_screen(max_x, full_x, timeline.left(), timeline.right());
                    if pointer.x < current_left || pointer.x > current_right {
                        let width = max_x - min_x;
                        let center = timeline_screen_to_value(pointer.x, timeline, full_x);
                        (min_x, max_x) =
                            clamp_x_range(center - width * 0.5, center + width * 0.5, full_x);
                    }
                    self.x_range = Some((min_x, max_x));
                    ui.ctx().request_repaint();
                } else if timeline_response.dragged() && self.timeline_dragging {
                    if let Some(pointer) = timeline_response.interact_pointer_pos() {
                        let width = max_x - min_x;
                        let center = timeline_screen_to_value(pointer.x, timeline, full_x);
                        (min_x, max_x) =
                            clamp_x_range(center - width * 0.5, center + width * 0.5, full_x);
                        self.x_range = Some((min_x, max_x));
                        ui.ctx().request_repaint();
                    }
                } else if timeline_response.clicked()
                    && let Some(pointer) = timeline_response.interact_pointer_pos()
                {
                    let current_left =
                        value_to_screen(min_x, full_x, timeline.left(), timeline.right());
                    let current_right =
                        value_to_screen(max_x, full_x, timeline.left(), timeline.right());
                    if pointer.x < current_left || pointer.x > current_right {
                        let width = max_x - min_x;
                        let center = full_x.0
                            + ((pointer.x - timeline.left()) / timeline.width()).clamp(0., 1.)
                                as f64
                                * (full_x.1 - full_x.0);
                        (min_x, max_x) =
                            clamp_x_range(center - width * 0.5, center + width * 0.5, full_x);
                        self.x_range = Some((min_x, max_x));
                        ui.ctx().request_repaint();
                    } else {
                        self.x_range = Some(clamp_x_range(min_x, max_x, full_x));
                    }
                } else {
                    if timeline_response.drag_stopped() {
                        self.timeline_dragging = false;
                    }
                    self.x_range = Some(clamp_x_range(min_x, max_x, full_x));
                }
            }
            let (min_x, max_x) = self.x_range.unwrap();
            let padding = (max_x - min_x) * 0.2;
            let selected_count = self
                .selected
                .iter()
                .filter(|&&selected| selected)
                .count()
                .max(1);
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
            painter.rect_stroke(
                rect,
                0.,
                Stroke::new(1., Color32::DARK_GRAY),
                egui::StrokeKind::Inside,
            );
            painter.rect_filled(timeline, 0., Color32::from_rgb(14, 16, 19));
            painter.rect_stroke(
                timeline,
                0.,
                Stroke::new(1., Color32::DARK_GRAY),
                egui::StrokeKind::Inside,
            );
            let plot_area =
                Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.bottom() - 26.));
            let row_h = plot_area.height() / groups.len().max(1) as f32;
            for (row, group) in groups.iter().enumerate() {
                let row_rect = Rect::from_min_max(
                    Pos2::new(plot_area.left(), plot_area.top() + row as f32 * row_h),
                    Pos2::new(
                        plot_area.right(),
                        plot_area.top() + (row + 1) as f32 * row_h,
                    ),
                )
                .shrink2(Vec2::new(64., 10.));
                let y_bounds = self.group_bounds(group);
                if row_rect.height() >= 8. {
                    draw_grid(painter, row_rect, (min_x, max_x), y_bounds);
                    painter.line_segment(
                        [
                            Pos2::new(row_rect.left(), row_rect.center().y),
                            Pos2::new(row_rect.right(), row_rect.center().y),
                        ],
                        Stroke::new(1., Color32::from_gray(50)),
                    );
                }
                let unit = group
                    .iter()
                    .map(|i| self.trace_unit(*i))
                    .next()
                    .unwrap_or("");
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
            painter.rect_stroke(
                Rect::from_min_max(
                    Pos2::new(sel_l, timeline.top()),
                    Pos2::new(sel_r, timeline.bottom()),
                ),
                0.,
                Stroke::new(1., Color32::WHITE),
                egui::StrokeKind::Inside,
            );
            let x_unit = data
                .scale()
                .map(|i| data.variables[i].unit.as_str())
                .unwrap_or("");
            draw_x_axis(
                painter,
                plot_area.shrink2(Vec2::new(64., 0.)),
                (min_x, max_x),
                x_unit,
                self.log_x,
            );

            if response.hovered()
                && let Some(pointer) = response.hover_pos()
                && let Some((trace, point, screen)) =
                    self.closest_trace(&groups, plot_area, pointer, (min_x, max_x))
            {
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
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "x = {}\ny = {}",
                                        si_value(
                                            if self.log_x {
                                                10_f64.powf(point.x)
                                            } else {
                                                point.x
                                            },
                                            x_unit,
                                        ),
                                        si_value(point.y, y_unit)
                                    ))
                                    .monospace(),
                                )
                                .extend(),
                            );
                        });
                    });
            }
        });
    }

    fn trace_groups(&self) -> Vec<Vec<usize>> {
        let selected: Vec<_> = self
            .traces
            .keys()
            .copied()
            .filter(|i| self.selected[*i])
            .collect();
        match self.group_mode {
            GroupMode::Overlay => vec![selected],
            GroupMode::Separate => selected.into_iter().map(|i| vec![i]).collect(),
            GroupMode::ByUnit => {
                let mut map: BTreeMap<String, Vec<usize>> = BTreeMap::new();
                for i in selected {
                    map.entry(self.trace_unit(i).to_owned())
                        .or_default()
                        .push(i);
                }
                map.into_values().collect()
            }
        }
        .into_iter()
        .filter(|g| !g.is_empty())
        .collect()
    }
    fn trace_unit(&self, index: usize) -> &str {
        let variable = &self.raw.plots[self.plot].variables[index];
        match self.mode {
            Mode::Phase => "deg",
            Mode::Decibels => "dB",
            _ => variable.unit.as_str(),
        }
    }
    fn group_bounds(&self, group: &[usize]) -> (f64, f64) {
        if let Some(range) = self.y_range { return range; }
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        for &i in group {
            let b = self.traces[&i].series.bounds;
            min = min.min(b.min()[1]);
            max = max.max(b.max()[1]);
        }
        if !min.is_finite() || !max.is_finite() {
            (-1., 1.)
        } else if min == max {
            (min - 1., max + 1.)
        } else {
            (min, max)
        }
    }
    fn closest_trace<'a>(
        &'a self,
        groups: &[Vec<usize>],
        rect: Rect,
        pointer: Pos2,
        x_bounds: (f64, f64),
    ) -> Option<(&'a Trace, PlotPoint, Pos2)> {
        let row_h = rect.height() / groups.len().max(1) as f32;
        groups
            .iter()
            .enumerate()
            .flat_map(|(row, group)| {
                let row_rect = Rect::from_min_max(
                    Pos2::new(rect.left(), rect.top() + row as f32 * row_h),
                    Pos2::new(rect.right(), rect.top() + (row + 1) as f32 * row_h),
                )
                .shrink2(Vec2::new(64., 10.));
                let y_bounds = self.group_bounds(group);
                group.iter().filter_map(move |i| {
                    let trace = &self.traces[i];
                    closest_point_on_rendered_trace(trace, pointer, row_rect, x_bounds, y_bounds)
                        .map(|(point, screen, distance)| (trace, point, screen, distance))
                })
            })
            .min_by(|a, b| a.3.total_cmp(&b.3))
            .filter(|c| c.3 <= 18.)
            .map(|(a, b, c, _)| (a, b, c))
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
            self.fit = self.x_range.is_none();
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
        let aliases = Arc::clone(&self.aliases);
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
                traces.push(Trace::new(&raw.plots[plot], i, mode, log_x, &aliases[plot]));
            }
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            let _ = tx.send(traces);
            ctx.request_repaint_of(egui::ViewportId::from_hash_of("fschem-waveforms"));
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
        [
            Pos2::new(axis_x, row.top()),
            Pos2::new(axis_x, row.bottom()),
        ],
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
            format!(
                "{}{}",
                fixed_scaled_number(value, scale, precision),
                axis_unit
            ),
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
    let left = plot_area.left();
    let right = plot_area.right();
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
            format!(
                "{}{}",
                fixed_scaled_number(display_value, scale, precision),
                axis_unit
            ),
            font.clone(),
            color,
        );
    }
}

fn draw_grid(painter: &egui::Painter, rect: Rect, x: (f64, f64), y: (f64, f64)) {
    let faint_stroke = Stroke::new(1., Color32::from_gray(26));
    let stroke = Stroke::new(1., Color32::from_gray(34));
    let major_stroke = Stroke::new(1., Color32::from_gray(42));

    let x_step = nice_step(x.1 - x.0, (rect.width() / 90.).max(2.) as f64);
    let x_sub_step = x_step / 5.;
    let first_x_sub = (x.0 / x_sub_step).ceil() as i64;
    let last_x_sub = (x.1 / x_sub_step).floor() as i64;
    for tick in first_x_sub..=last_x_sub {
        if tick % 5 == 0 {
            continue;
        }
        let value = tick as f64 * x_sub_step;
        let sx = value_to_screen(value, x, rect.left(), rect.right());
        painter.line_segment(
            [Pos2::new(sx, rect.top()), Pos2::new(sx, rect.bottom())],
            faint_stroke,
        );
    }
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
    let y_sub_step = y_step / 5.;
    let first_y_sub = (y.0 / y_sub_step).ceil() as i64;
    let last_y_sub = (y.1 / y_sub_step).floor() as i64;
    for tick in first_y_sub..=last_y_sub {
        if tick % 5 == 0 {
            continue;
        }
        let value = tick as f64 * y_sub_step;
        let sy = value_to_screen(value, y, rect.bottom(), rect.top());
        painter.line_segment(
            [Pos2::new(rect.left(), sy), Pos2::new(rect.right(), sy)],
            faint_stroke,
        );
    }
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

fn timeline_screen_to_value(x: f32, timeline: Rect, full: (f64, f64)) -> f64 {
    full.0 + ((x - timeline.left()) / timeline.width()).clamp(0., 1.) as f64 * (full.1 - full.0)
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
fn screen_to_value(value: f32, bounds: (f64, f64), min: f32, max: f32) -> f64 {
    let denom = max - min;
    if denom == 0. || !denom.is_finite() {
        return (bounds.0 + bounds.1) * 0.5;
    }
    bounds.0 + ((value - min) / denom) as f64 * (bounds.1 - bounds.0)
}
fn point_to_screen(point: PlotPoint, rect: Rect, x: (f64, f64), y: (f64, f64)) -> Pos2 {
    Pos2::new(
        value_to_screen(point.x, x, rect.left(), rect.right()),
        value_to_screen(point.y, y, rect.bottom(), rect.top()),
    )
}
fn closest_point_on_rendered_trace(
    trace: &Trace,
    pointer: Pos2,
    rect: Rect,
    x: (f64, f64),
    y: (f64, f64),
) -> Option<(PlotPoint, Pos2, f32)> {
    let mut best: Option<(PlotPoint, Pos2, f32)> = None;
    for segment in &trace.series.rendered {
        for point in segment {
            let screen = point_to_screen(*point, rect, x, y);
            let distance2 = screen.distance_sq(pointer);
            if best.is_none_or(|(_, _, best_distance2)| distance2 < best_distance2) {
                best = Some((*point, screen, distance2));
            }
        }
        for points in segment.windows(2) {
            let a = points[0];
            let b = points[1];
            let a_screen = point_to_screen(a, rect, x, y);
            let b_screen = point_to_screen(b, rect, x, y);
            let ab = b_screen - a_screen;
            let ab_len2 = ab.length_sq();
            if ab_len2 <= f32::EPSILON {
                continue;
            }
            let t = ((pointer - a_screen).dot(ab) / ab_len2).clamp(0., 1.);
            let screen = a_screen + ab * t;
            let point = PlotPoint::new(a.x + (b.x - a.x) * t as f64, a.y + (b.y - a.y) * t as f64);
            let distance2 = screen.distance_sq(pointer);
            if best.is_none_or(|(_, _, best_distance2)| distance2 < best_distance2) {
                best = Some((point, screen, distance2));
            }
        }
    }
    best.or_else(|| {
        let cursor = PlotPoint::new(
            screen_to_value(pointer.x, x, rect.left(), rect.right()),
            screen_to_value(pointer.y, y, rect.bottom(), rect.top()),
        );
        let point = trace.series.interpolate(cursor)?;
        let screen = point_to_screen(point, rect, x, y);
        Some((point, screen, screen.distance_sq(pointer)))
    })
    .map(|(point, screen, distance2)| (point, screen, distance2.sqrt()))
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
        let points: Vec<_> = segment
            .iter()
            .map(|p| point_to_screen(*p, rect, x, y))
            .collect();
        if points.len() >= 2 {
            painter.add(egui::Shape::line(
                points,
                Stroke::new(1., color.gamma_multiply(0.75)),
            ));
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
        let points: Vec<_> = segment
            .iter()
            .map(|p| point_to_screen(*p, rect, x, y))
            .collect();
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
    fn linked_configuration_and_stacked_render_preserve_selection_and_limits() {
        let raw = RawFile::parse(b"Title: test\nPlotname: Transient\nFlags: real\nNo. Variables: 3\nNo. Points: 2\nVariables:\n0 time time\n1 v(out) voltage\n2 i(v1) current\nValues:\n0 0\n2\n0.01\n1 1\n3\n0.02\n").unwrap();
        let mut display = Display::new("missing.raw".into(), raw);
        let props = fschem::document::Properties::parse("node=\"out i(v1)\" groupmode=separate logx=0 x1=0.2 x2=0.8 y1=-2 y2=4 mode=real".into());
        display.configure(&props);
        assert_eq!(display.x_range(), Some((0.2, 0.8)));
        assert_eq!(display.group_bounds(&[1]), (-2., 4.));
        assert_eq!(display.configuration()["groupmode"], "separate");
        assert_eq!(display.configuration()["node"], "v(out) i(v1)");
        let selected = display.selected.clone();
        let ctx = egui::Context::default();
        for _ in 0..50 {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| display.paint_embedded(ui, Rect::from_min_size(Pos2::ZERO, Vec2::new(600., 400.)), &props));
            });
            if display.traces.len() == 2 { break; }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(display.selected, selected);
        assert_eq!(display.trace_groups(), vec![vec![1], vec![2]]);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| display.paint_embedded(ui, Rect::from_min_size(Pos2::ZERO, Vec2::new(600., 400.)), &props));
        });
        let labels: Vec<_> = output.shapes.into_iter().filter_map(|shape| match shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text.contains("v(out)") || text.galley.job.text.contains("i(v1)") => Some(text.pos.y), _ => None,
        }).collect();
        assert_eq!(labels.len(), 2);
        assert!(labels[1] - labels[0] > 150., "traces occupy different panels");
        display.configure(&fschem::document::Properties::parse("node=out".into()));
        assert_eq!(display.group_mode, GroupMode::Overlay);
        assert_eq!(display.x_range(), None);
        assert_eq!(display.y_range, None);
    }

    #[test]
    fn raw_open_discovers_matching_manifest_aliases_only() {
        let root = std::env::temp_dir().join(format!("fschem-graph-alias-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("op.raw");
        std::fs::write(&path, b"Title: test\nPlotname: Operating Point\nFlags: real\nNo. Variables: 1\nNo. Points: 1\nVariables:\n0 v(out) voltage\nValues:\n0 2\n").unwrap();
        let mut manifest = serde_json::json!({"schema_version":1,"status":"succeeded","backend":"ngspice","mode":"portable","source":"/source.sch","analyses":{},"artifacts":{"deck":"circuit.spice","original_deck":"original.deck","source_snapshot":"source.sch","log":"simulator.log","inputs":[]},"diagnostics":[],"results":[{"id":"op.raw#0","path":"op.raw","plot_index":0,"name":"Operating Point","analysis_name":"bias","signals":["v(out)"],"signal_map":{"v(out)":"v(out)","/OUT":"v(out)"}}]});
        let manifest_path = root.join("run.json");
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        let display = Display::new(path.clone(), RawFile::open(&path).unwrap());
        assert_eq!(display.aliases[0]["/OUT"], "v(out)");
        assert!(display.traces.values().any(|trace| trace.label.contains("/OUT (v(out))")));
        manifest["results"][0]["name"] = "Wrong plot".into();
        std::fs::write(&manifest_path, manifest.to_string()).unwrap();
        assert!(Display::new(path.clone(), RawFile::open(&path).unwrap()).aliases[0].is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn canonical_aliases_preserve_native_names_and_configure_graphs() {
        let raw = RawFile::parse(b"Title: test\nPlotname: Operating Point\nFlags: real\nNo. Variables: 2\nNo. Points: 1\nVariables:\n0 v(top:out) voltage\n1 v(top.out) voltage\nValues:\n0 2\n3\n").unwrap();
        let mut display = Display::new("no-manifest.raw".into(), raw);
        display.aliases = Arc::new(vec![BTreeMap::from([("/top/out".into(), "v(top:out)".into())])]);
        display.configure(&fschem::document::Properties::parse("node=/top/out".into()));
        assert_eq!(display.selected, vec![true, false]);
        assert_eq!(signal_label("v(top:out)", &display.aliases[0]), "/top/out (v(top:out))");
        assert_eq!(signal_label("v(top.out)", &display.aliases[0]), "v(top.out)");
        assert_eq!(display.raw.plots[0].variables[0].name, "v(top:out)");
        assert!(display.open_plot(5).is_err());
    }

    #[test]
    fn reload_preserves_selection_by_name_and_view() {
        let raw = |variables: &str| {
            RawFile::parse(format!("Title: test\nPlotname: Operating Point\nFlags: real\nNo. Variables: 2\nNo. Points: 1\nVariables:\n{variables}\nValues:\n0 2\n3\n").as_bytes()).unwrap()
        };
        let mut previous =
            Display::new("test.raw".into(), raw("0 v(out) voltage\n1 v(in) voltage"));
        previous.x_range = Some((0.25, 0.75));
        previous.fit = false;
        let mut next = Display::new("test.raw".into(), raw("0 v(in) voltage\n1 v(out) voltage"));
        next.restore_view(&previous);
        assert_eq!(next.selected, vec![false, true]);
        assert_eq!(next.x_range, previous.x_range);
        assert!(!next.fit);
        assert!(next.traces.is_empty());
        assert!(next.dirty);
    }

    #[test]
    fn schematic_signals_render_with_shared_projection_and_lod() {
        let raw = RawFile::parse(b"Title: test\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 3\nNo. Points: 2\nVariables:\n0 time time\n1 v(in) voltage\n2 v(out) voltage\nValues:\n0 0\n1\n2\n1 1\n3\n4\n").unwrap();
        let mut display = Display::new("test.raw".into(), raw);
        let props = fschem::document::Properties::parse("node=out x1=0 x2=1 y1=0 y2=5".into());
        display.configure(&props);
        assert_eq!(display.selected, vec![false, false, true]);
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            display.prepare_traces(&ctx);
            if display.preparing.is_none() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                display.paint_embedded(
                    ui,
                    Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 250.)),
                    &props,
                );
            });
        });
        assert!(
            !ctx.tessellate(output.shapes, output.pixels_per_point)
                .is_empty()
        );
        assert_eq!(
            display.traces[&2].series.rendered[0][0],
            PlotPoint::new(0., 2.)
        );
    }

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
