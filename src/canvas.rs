use eframe::egui::{
    self, Color32, Pos2, Rect, Stroke,
    epaint::{Mesh, Vertex},
};
use fschem::{
    geometry::{Bounds, Point, Viewport, wire_route},
    scene::{Fill, Scene, Selection, Shape, Style},
    stroke_font::StrokeFont,
};
use std::collections::{BTreeMap, BTreeSet};

pub const COLORS: [Color32; 22] = [
    Color32::BLACK,
    Color32::from_rgb(0, 221, 255),
    Color32::from_gray(70),
    Color32::from_gray(204),
    Color32::from_rgb(136, 221, 0),
    Color32::from_rgb(187, 34, 0),
    Color32::from_rgb(0, 170, 255),
    Color32::RED,
    Color32::YELLOW,
    Color32::WHITE,
    Color32::MAGENTA,
    Color32::GREEN,
    Color32::from_rgb(0, 68, 255),
    Color32::from_rgb(170, 170, 0),
    Color32::from_rgb(170, 204, 170),
    Color32::from_rgb(255, 119, 119),
    Color32::from_rgb(191, 255, 129),
    Color32::from_rgb(0, 255, 204),
    Color32::from_rgb(206, 0, 151),
    Color32::from_rgb(210, 212, 107),
    Color32::from_rgb(239, 97, 88),
    Color32::from_rgb(253, 178, 0),
];

pub fn layer_color(layer: u16) -> Color32 {
    // Additional custom layers should not accidentally cycle to black.
    COLORS[if layer == 0 {
        0
    } else {
        1 + (layer as usize - 1) % (COLORS.len() - 1)
    }]
}

/// Triangulate static geometry when loading, not on every mouse movement.
pub struct PaintCache {
    triangles: Vec<Vec<u32>>,
    text_segments: Vec<Vec<(Point, Point)>>,
}
impl PaintCache {
    pub fn new(scene: &Scene) -> Self {
        let font = StrokeFont::bundled();
        let triangles = scene
            .items
            .iter()
            .map(|item| {
                if let Shape::Path {
                    points,
                    closed: true,
                } = &item.shape
                    && item.style.fill != Fill::None
                    && points.len() >= 3
                {
                    let coords: Vec<_> = points.iter().flat_map(|p| [p.x, p.y]).collect();
                    return earcutr::earcut(&coords, &[], 2)
                        .unwrap_or_default()
                        .into_iter()
                        .map(|i| i as u32)
                        .collect();
                }
                Vec::new()
            })
            .collect();
        let text_segments = scene
            .items
            .iter()
            .map(|item| {
                if let Shape::Text(run) = &item.shape {
                    let mut segments = Vec::new();
                    font.lines(run, |a, b| segments.push((a, b)));
                    segments
                } else {
                    Vec::new()
                }
            })
            .collect();
        Self {
            triangles,
            text_segments,
        }
    }
}

pub struct Canvas<'a> {
    pub painter: &'a egui::Painter,
    pub rect: Rect,
    pub viewport: Viewport,
}
impl Canvas<'_> {
    pub fn size(&self) -> Point {
        Point::new(self.rect.width() as f64, self.rect.height() as f64)
    }
    pub fn screen(&self, point: Point) -> Pos2 {
        let p = self.viewport.screen(point, self.size());
        // GPU vertices must remain finite, even for geometry far outside the
        // viewport. Clip strokes in f64 before conversion; bound fill vertices.
        Pos2::new(
            self.rect.left() + p.x.clamp(-1e6, 1e6) as f32,
            self.rect.top() + p.y.clamp(-1e6, 1e6) as f32,
        )
    }
    pub fn world(&self, pos: Pos2) -> Point {
        self.viewport.world(
            Point::new(
                (pos.x - self.rect.left()) as f64,
                (pos.y - self.rect.top()) as f64,
            ),
            self.size(),
        )
    }
    fn world_bounds(&self) -> Bounds {
        Bounds::new(self.world(self.rect.min), self.world(self.rect.max))
    }
    pub fn grid(&self, mut spacing: f64) {
        if !spacing.is_finite() || spacing <= 0. {
            return;
        }
        while spacing * self.viewport.zoom < 14. {
            spacing *= 2.;
        }
        let bounds = self.world_bounds();
        while (bounds.size().x / spacing) * (bounds.size().y / spacing) > 8_000. {
            spacing *= 2.;
        }
        let first_x = (bounds.min.x / spacing).ceil() * spacing;
        let first_y = (bounds.min.y / spacing).ceil() * spacing;
        let nx = (bounds.size().x / spacing).ceil() as usize + 1;
        let ny = (bounds.size().y / spacing).ceil() as usize + 1;
        for x in 0..nx.min(2000) {
            for y in 0..ny.min(2000) {
                let point = self.screen(Point::new(
                    first_x + x as f64 * spacing,
                    first_y + y as f64 * spacing,
                ));
                self.painter.rect_filled(
                    Rect::from_center_size(point, egui::vec2(1., 1.)),
                    0.,
                    Color32::from_gray(45),
                );
            }
        }
        self.segment(
            Point::new(bounds.min.x, 0.),
            Point::new(bounds.max.x, 0.),
            Stroke::new(1., Color32::from_gray(45)),
            4.,
        );
        self.segment(
            Point::new(0., bounds.min.y),
            Point::new(0., bounds.max.y),
            Stroke::new(1., Color32::from_gray(45)),
            4.,
        );
    }
    pub fn scene(
        &self,
        scene: &Scene,
        cache: &PaintCache,
        layers: &BTreeMap<u16, bool>,
        show_hidden: bool,
        selected: &BTreeSet<Selection>,
    ) {
        let world_bounds = self.world_bounds();
        let visible = world_bounds.expanded(100. / self.viewport.zoom);
        let is_selected =
            |item: &fschem::scene::Item| item.selection().is_some_and(|id| selected.contains(&id));
        // Keep scene order within each group, but paint the selection on top.
        let items = scene.items.iter().enumerate();
        for (index, item) in items
            .clone()
            .filter(|(_, item)| !is_selected(item))
            .chain(items.filter(|(_, item)| is_selected(item)))
        {
            if (item.hidden && !show_hidden)
                || !layers.get(&item.layer).copied().unwrap_or(true)
                || !visible.intersects(item.bounds)
            {
                continue;
            }
            let color = if is_selected(item) {
                Color32::from_rgb(255, 185, 65)
            } else {
                layer_color(item.layer)
            };
            let width = stroke_width(&item.style, self.viewport.zoom);
            match &item.shape {
                Shape::Path { points, closed } => {
                    if !cache.triangles[index].is_empty() {
                        let color = if item.style.fill == Fill::Stipple && item.layer > 5 {
                            color.gamma_multiply(0.25)
                        } else {
                            color
                        };
                        let vertices = points
                            .iter()
                            .map(|&point| Vertex {
                                pos: self.screen(point),
                                uv: egui::epaint::WHITE_UV,
                                color,
                            })
                            .collect();
                        self.painter.add(Mesh {
                            vertices,
                            indices: cache.triangles[index].clone(),
                            ..Default::default()
                        });
                    }
                    for segment in points.windows(2) {
                        self.segment_in_bounds(
                            segment[0],
                            segment[1],
                            Stroke::new(width, color),
                            item.style.dash,
                            world_bounds,
                        );
                    }
                    if *closed && points.len() >= 2 {
                        self.segment_in_bounds(
                            *points.last().unwrap(),
                            points[0],
                            Stroke::new(width, color),
                            item.style.dash,
                            world_bounds,
                        );
                    }
                }
                Shape::Text(run) => {
                    if run.scale.y * self.viewport.zoom * 40. < 3. {
                        continue;
                    }
                    for &(a, b) in &cache.text_segments[index] {
                        self.segment_in_bounds(
                            a,
                            b,
                            Stroke::new(width.min(1.2), color),
                            0.,
                            world_bounds,
                        );
                    }
                }
            }
        }
    }
    pub fn wire_preview(&self, a: Point, b: Point, on_connection: bool) {
        let color = Color32::from_rgb(255, 185, 65);
        for segment in wire_route(a, b).windows(2) {
            self.segment(segment[0], segment[1], Stroke::new(1.5, color), 0.);
        }
        self.painter.circle_stroke(
            self.screen(b),
            if on_connection { 5. } else { 3. },
            Stroke::new(1., color),
        );
    }
    fn segment(&self, a: Point, b: Point, stroke: Stroke, dash: f64) {
        self.segment_in_bounds(a, b, stroke, dash, self.world_bounds());
    }
    fn segment_in_bounds(&self, a: Point, b: Point, stroke: Stroke, dash: f64, bounds: Bounds) {
        let Some((start, end)) = clip_segment(
            a,
            b,
            bounds.expanded(stroke.width as f64 / self.viewport.zoom),
        ) else {
            return;
        };
        let p = self.screen(start);
        let q = self.screen(end);
        if dash <= 0. {
            self.painter.line_segment([p, q], stroke);
            return;
        }
        let delta = q - p;
        let length = delta.length();
        if length <= 0. {
            return;
        }
        let unit = delta / length;
        let dash = dash.clamp(1., 256.) as f32;
        let phase = (((start - a).length() * self.viewport.zoom) % (dash as f64 * 2.)) as f32;
        let mut distance = -phase;
        while distance < length {
            if distance + dash > 0. {
                self.painter.line_segment(
                    [
                        p + unit * distance.max(0.),
                        p + unit * (distance + dash).min(length),
                    ],
                    stroke,
                );
            }
            distance += dash * 2.;
        }
    }
}

fn stroke_width(style: &Style, zoom: f64) -> f32 {
    if style.bus {
        3.
    } else if style.width > 0. {
        (style.width * zoom).clamp(0.7, 100.) as f32
    } else {
        1.
    }
}

fn clip_segment(a: Point, b: Point, bounds: Bounds) -> Option<(Point, Point)> {
    let d = b - a;
    let mut lo: f64 = 0.;
    let mut hi: f64 = 1.;
    for (p, q) in [
        (-d.x, a.x - bounds.min.x),
        (d.x, bounds.max.x - a.x),
        (-d.y, a.y - bounds.min.y),
        (d.y, bounds.max.y - a.y),
    ] {
        if p == 0. {
            if q < 0. {
                return None;
            }
        } else if p < 0. {
            lo = lo.max(q / p);
        } else {
            hi = hi.min(q / p);
        }
        if lo > hi {
            return None;
        }
    }
    Some((a + d * lo, a + d * hi))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clips_long_strokes_before_dashing_or_gpu_conversion() {
        let bounds = Bounds::new(Point::new(-10., -10.), Point::new(10., 10.));
        let (a, b) = clip_segment(Point::new(-100., 0.), Point::new(100., 0.), bounds).unwrap();
        assert!((a - Point::new(-10., 0.)).length() < 1e-9);
        assert!((b - Point::new(10., 0.)).length() < 1e-9);
        assert!(clip_segment(Point::new(-100., 20.), Point::new(100., 20.), bounds).is_none());
    }
}
