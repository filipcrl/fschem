//! Symbol-only primitive creation controls. Placement uses the normal move tool.
use eframe::egui;

pub(super) struct SymbolTools {
    layer: u16,
    width: f64,
    height: f64,
    radius: f64,
    text: String,
    scale: f64,
    pin_name: String,
    pin_direction: String,
}
impl Default for SymbolTools {
    fn default() -> Self {
        Self {
            layer: 4, width: 40., height: 40., radius: 20.,
            text: "@name".into(), scale: 0.3,
            pin_name: "P1".into(), pin_direction: "inout".into(),
        }
    }
}

fn escaped(text: &str) -> String {
    text.chars().flat_map(|c| {
        if matches!(c, '\\' | '{' | '}' | '"') { vec!['\\', c] } else { vec![c] }
    }).collect()
}

impl SymbolTools {
    pub(super) fn show(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut primitive = None;
        ui.heading("Symbol tools");
        ui.weak("Set dimensions, choose a tool, then click to place. Esc cancels; R rotates; X/Y mirror.");
        ui.horizontal(|ui| {
            ui.label("Drawing layer");
            ui.add(egui::DragValue::new(&mut self.layer).range(0..=255));
        });
        ui.horizontal(|ui| {
            ui.label("Width / height");
            ui.add(egui::DragValue::new(&mut self.width).range(1.0..=1e6));
            ui.add(egui::DragValue::new(&mut self.height).range(1.0..=1e6));
        });
        ui.horizontal(|ui| {
            if ui.button("Line").clicked() {
                primitive = Some(format!("L {} 0 0 {} 0 {{}}\n", self.layer, self.width));
            }
            if ui.button("Rectangle").clicked() {
                primitive = Some(format!("B {} 0 0 {} {} {{}}\n", self.layer, self.width, self.height));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Radius");
            ui.add(egui::DragValue::new(&mut self.radius).range(1.0..=1e6));
            if ui.button("Circle").clicked() {
                primitive = Some(format!("A {} 0 0 {} 0 360 {{}}\n", self.layer, self.radius));
            }
        });
        ui.separator();
        ui.label("Text (supports @name and symbol attributes)");
        ui.text_edit_singleline(&mut self.text);
        ui.horizontal(|ui| {
            ui.label("Scale");
            ui.add(egui::DragValue::new(&mut self.scale).speed(0.05).range(0.05..=100.));
            if ui.add_enabled(!self.text.trim().is_empty(), egui::Button::new("Add text")).clicked() {
                primitive = Some(format!("T {{{}}} 0 0 0 0 {} {} {{layer={}}}\n", escaped(&self.text), self.scale, self.scale, self.layer));
            }
        });
        ui.separator();
        ui.label("Pin name");
        ui.text_edit_singleline(&mut self.pin_name);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("symbol_pin_direction").selected_text(&self.pin_direction).show_ui(ui, |ui| {
                for direction in ["in", "out", "inout"] {
                    ui.selectable_value(&mut self.pin_direction, direction.into(), direction);
                }
            });
            if ui.add_enabled(!self.pin_name.trim().is_empty(), egui::Button::new("Add pin")).clicked() {
                primitive = Some(format!("B 5 -2.5 -2.5 2.5 2.5 {{name=\"{}\" dir={}}}\n", escaped(&self.pin_name), self.pin_direction));
            }
        });
        primitive
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_text_and_quoted_pin_names() {
        let text = "a {b} \\ \"c\"";
        let source = format!("T {{{}}} 0 0 0 0 0.3 0.3 {{}}\n", super::escaped(text));
        let doc = fschem::document::Document::parse(source).unwrap();
        assert!(matches!(&doc.records[0].kind, fschem::document::RecordKind::Text { text: parsed, .. } if parsed == text));
    }
}
