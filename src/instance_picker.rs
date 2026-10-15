use eframe::egui::{self, Key};
use fschem::library::LibraryPaths;
use std::{collections::BTreeSet, path::PathBuf};

pub(super) struct InstancePicker {
    entries: Vec<PathBuf>,
    query: String,
    selected: usize,
    focus: bool,
}

// Ordered subsequence matching, rewarding contiguous matches and short paths.
fn score(candidate: &str, query: &str) -> Option<usize> {
    let candidate = candidate.to_lowercase();
    let mut offset = 0;
    let mut penalty = candidate.len();
    for c in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let gap = candidate[offset..].find(c)?;
        penalty += gap * 4;
        offset += gap + c.len_utf8();
    }
    Some(penalty)
}

impl InstancePicker {
    pub(super) fn new(paths: &LibraryPaths) -> Self {
        let mut pending = paths.roots.clone();
        let mut visited = BTreeSet::new();
        let mut entries = BTreeSet::new();
        while let Some(path) = pending.pop() {
            let Ok(path) = path.canonicalize() else { continue };
            if !visited.insert(path.clone()) { continue; }
            if path.is_dir() {
                if let Ok(children) = std::fs::read_dir(path) {
                    pending.extend(children.flatten().map(|e| e.path()));
                }
            } else if path.extension().is_some_and(|e| e == "sym") {
                entries.insert(path);
            }
        }
        Self { entries: entries.into_iter().collect(), query: String::new(), selected: 0, focus: true }
    }

    pub(super) fn show(&mut self, ctx: &egui::Context) -> (bool, Option<PathBuf>) {
        let mut open = true;
        let mut chosen = None;
        let escape = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        egui::Window::new("Place library instance")
            .open(&mut open).collapsible(false).default_width(560.)
            .show(ctx, |ui| {
                let search = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search symbols…").desired_width(f32::INFINITY));
                if self.focus { search.request_focus(); self.focus = false; }
                if search.changed() { self.selected = 0; }
                let mut matches: Vec<_> = self.entries.iter().filter_map(|path| {
                    // Search library-relative names, not the machine-specific root.
                    let name = path.file_name()?.to_string_lossy();
                    score(&name, &self.query).map(|score| (score, path))
                }).collect();
                matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
                let down = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowDown));
                let up = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowUp));
                if down { self.selected += 1; }
                if up { self.selected = self.selected.saturating_sub(1); }
                self.selected = self.selected.min(matches.len().saturating_sub(1));
                let enter = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter));
                if enter { chosen = matches.get(self.selected).map(|(_, p)| (*p).clone()); }
                ui.label(format!("{} symbols · ↑/↓ select · Enter to place · Esc to cancel", matches.len()));
                egui::ScrollArea::vertical().max_height(360.).show(ui, |ui| {
                    for (index, (_, path)) in matches.iter().enumerate() {
                        let response = ui.selectable_label(index == self.selected, path.file_name().unwrap().to_string_lossy());
                        if index == self.selected && (down || up || search.changed()) { response.scroll_to_me(None); }
                        if response.clicked() { chosen = Some((*path).clone()); }
                        response.on_hover_text(path.display().to_string());
                    }
                    if matches.is_empty() { ui.label("No matching symbols. Check your library paths."); }
                });
            });
        (!open || escape, chosen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuzzy_subsequences_are_ranked() {
        assert!(score("resistor.sym", "rstr").is_some());
        assert!(score("resistor.sym", "xyz").is_none());
        assert!(score("res.sym", "res") < score("resistor.sym", "res"));
        assert_eq!(score("RES.sym", "rs"), score("res.sym", "RS"));
    }
}
