//! Schematic graph bindings and asynchronous raw-file refresh.
use crate::{canvas::Canvas, graph::Display};
use eframe::egui::{self, Color32};
use fschem::{
    document::{Properties, RecordKind},
    scene::LoadedDocument,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, PartialEq)]
struct Stamp(SystemTime, u64);
fn stamp(path: &Path) -> Option<Stamp> {
    let m = path.metadata().ok()?;
    Some(Stamp(m.modified().ok()?, m.len()))
}

struct Binding {
    path: PathBuf,
    properties: String,
    stamp: Option<Stamp>,
    checked: Option<Instant>,
    loading: Option<mpsc::Receiver<Result<Display, String>>>,
    data: Option<Display>,
    error: Option<String>,
}
impl Binding {
    fn refresh(&mut self, ctx: &egui::Context, props: &Properties) {
        if let Some(result) = self.loading.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.loading = None;
            match result {
                Ok(data) => {
                    self.data = Some(data);
                    self.error = None;
                }
                Err(error) => {
                    self.error = Some(error);
                    self.stamp = None;
                }
            }
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        if self.loading.is_some()
            || self
                .checked
                .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
        {
            return;
        }
        self.checked = Some(Instant::now());
        let next = stamp(&self.path);
        if next.is_none() {
            self.stamp = None;
            self.data = None;
            self.error = Some(format!("Raw file not found: {}", self.path.display()));
            return;
        }
        if self.stamp == next {
            return;
        }
        self.stamp = next;
        let (tx, rx) = mpsc::channel();
        self.loading = Some(rx);
        let path = self.path.clone();
        let props = props.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = fschem::raw::RawFile::open(&path).map(|raw| {
                let mut display = Display::new(path, raw);
                display.configure(&props);
                display
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
}

pub(crate) fn raw_path(document: Option<&Path>, props: &Properties) -> Option<PathBuf> {
    if let Some(file) = props.get("rawfile").filter(|s| !s.trim().is_empty()) {
        let path = PathBuf::from(file);
        return Some(if path.is_absolute() {
            path
        } else {
            document
                .and_then(Path::parent)
                .unwrap_or(Path::new("."))
                .join(path)
        });
    }
    document.map(|p| p.with_extension("raw"))
}

#[derive(Default)]
pub(crate) struct SchematicGraphs {
    bindings: BTreeMap<usize, Binding>,
}
impl SchematicGraphs {
    pub(crate) fn paint(
        &mut self,
        ui: &egui::Ui,
        canvas: &Canvas<'_>,
        loaded: &LoadedDocument,
        layers: &BTreeMap<u16, bool>,
    ) {
        let mut live = Vec::new();
        for (index, record) in loaded.document.records.iter().enumerate() {
            let RecordKind::Rectangle {
                layer,
                a,
                b,
                properties,
            } = &record.kind
            else {
                continue;
            };
            if !properties
                .get("flags")
                .is_some_and(|s| s.split_whitespace().any(|s| s == "graph"))
            {
                continue;
            }
            live.push(index);
            if layers.get(layer) == Some(&false) {
                continue;
            }
            let rect = egui::Rect::from_two_pos(canvas.screen(*a), canvas.screen(*b)).shrink(1.);
            if !rect.intersects(ui.clip_rect()) {
                continue;
            }
            let Some(path) = raw_path(loaded.path.as_deref(), properties) else {
                continue;
            };
            let binding = self.bindings.entry(index).or_insert_with(|| Binding {
                path: path.clone(),
                properties: properties.raw.clone(),
                stamp: None,
                checked: None,
                loading: None,
                data: None,
                error: None,
            });
            if binding.path != path || binding.properties != properties.raw {
                *binding = Binding {
                    path,
                    properties: properties.raw.clone(),
                    stamp: None,
                    checked: None,
                    loading: None,
                    data: None,
                    error: None,
                };
            }
            binding.refresh(ui.ctx(), properties);
            if let Some(data) = &mut binding.data {
                data.paint_embedded(ui, rect, properties);
            }
            if binding.data.is_none() || binding.error.is_some() {
                let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    binding.error.as_deref().unwrap_or("Loading waveforms…"),
                    egui::FontId::proportional(12.),
                    Color32::LIGHT_GRAY,
                );
            }
        }
        self.bindings.retain(|index, _| live.contains(index));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watches_creation_replacement_partial_writes_and_deletion() {
        let path = std::env::temp_dir().join(format!(
            "fschem-graph-{}-{}.raw",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let props = Properties::parse("node=out".into());
        let ctx = egui::Context::default();
        let mut binding = Binding {
            path: path.clone(),
            properties: props.raw.clone(),
            stamp: None,
            checked: None,
            loading: None,
            data: None,
            error: None,
        };
        binding.refresh(&ctx, &props);
        assert!(binding.error.is_some());
        let raw = b"Title: test\nPlotname: Transient Analysis\nFlags: real\nNo. Variables: 2\nNo. Points: 1\nVariables:\n0 time time\n1 v(out) voltage\nValues:\n0 0\n2\n";
        let finish = |binding: &mut Binding| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while binding.loading.is_some() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
                binding.refresh(&ctx, &props);
            }
        };
        std::fs::write(&path, raw).unwrap();
        binding.checked = None;
        binding.refresh(&ctx, &props);
        finish(&mut binding);
        assert!(binding.data.is_some());
        assert!(binding.error.is_none());
        std::fs::write(&path, b"partial write").unwrap();
        binding.checked = None;
        binding.refresh(&ctx, &props);
        finish(&mut binding);
        assert!(binding.error.is_some());
        assert!(
            binding.data.is_some(),
            "keep last valid data during a partial write"
        );
        std::fs::write(&path, raw).unwrap();
        binding.checked = None;
        binding.refresh(&ctx, &props);
        finish(&mut binding);
        assert!(binding.error.is_none());
        std::fs::remove_file(&path).unwrap();
        binding.checked = None;
        binding.refresh(&ctx, &props);
        assert!(binding.data.is_none());
    }

    #[test]
    fn resolves_raw_paths_relative_to_schematic() {
        let doc = Path::new("/tmp/design/circuit.sch");
        assert_eq!(
            raw_path(Some(doc), &Properties::parse(String::new())),
            Some(doc.with_extension("raw"))
        );
        assert_eq!(
            raw_path(
                Some(doc),
                &Properties::parse("rawfile=results/run.raw".into())
            ),
            Some(PathBuf::from("/tmp/design/results/run.raw"))
        );
        assert_eq!(raw_path(None, &Properties::parse(String::new())), None);
    }
}
