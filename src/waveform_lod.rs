//! Screen-space level of detail for waveforms. A compact extrema pyramid makes
//! overview queries proportional to screen width, not simulation sample count.
//! Only original samples are emitted, in their original order.
use std::ops::Range;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlotPoint {
    pub x: f64,
    pub y: f64,
}
impl PlotPoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlotBounds {
    min: [f64; 2],
    max: [f64; 2],
}
impl PlotBounds {
    pub const NOTHING: Self = Self {
        min: [f64::INFINITY, f64::INFINITY],
        max: [f64::NEG_INFINITY, f64::NEG_INFINITY],
    };
    pub fn from_min_max(min: [f64; 2], max: [f64; 2]) -> Self {
        Self { min, max }
    }
    pub fn min(&self) -> [f64; 2] {
        self.min
    }
    pub fn max(&self) -> [f64; 2] {
        self.max
    }
    pub fn merge(&mut self, other: &Self) {
        self.extend_with_x(other.min[0]);
        self.extend_with_x(other.max[0]);
        self.extend_with_y(other.min[1]);
        self.extend_with_y(other.max[1]);
    }
    pub fn extend_with_x(&mut self, x: f64) {
        if x.is_finite() {
            self.min[0] = self.min[0].min(x);
            self.max[0] = self.max[0].max(x);
        }
    }
    pub fn extend_with_y(&mut self, y: f64) {
        if y.is_finite() {
            self.min[1] = self.min[1].min(y);
            self.max[1] = self.max[1].max(y);
        }
    }
    pub fn is_finite(&self) -> bool {
        self.min.into_iter().chain(self.max).all(f64::is_finite)
    }
    pub fn is_valid_x(&self) -> bool {
        self.min[0].is_finite() && self.max[0].is_finite() && self.min[0] < self.max[0]
    }
}

const LEAF: usize = 64;

#[derive(Clone, Copy)]
struct Extrema {
    min: usize,
    max: usize,
}

struct Run {
    points: Vec<PlotPoint>,
    levels: Vec<Vec<Extrema>>,
    ascending: bool,
}
impl Run {
    fn new(points: Vec<PlotPoint>) -> Self {
        let ascending = points.last().unwrap().x >= points[0].x;
        let mut leaves = Vec::with_capacity(points.len().div_ceil(LEAF));
        for start in (0..points.len()).step_by(LEAF) {
            let mut extrema = Extrema {
                min: start,
                max: start,
            };
            for i in start + 1..(start + LEAF).min(points.len()) {
                if points[i].y < points[extrema.min].y {
                    extrema.min = i;
                }
                if points[i].y > points[extrema.max].y {
                    extrema.max = i;
                }
            }
            leaves.push(extrema);
        }
        let mut levels = vec![leaves];
        while levels.last().unwrap().len() > 1 {
            let previous = levels.last().unwrap();
            let next = previous
                .chunks(2)
                .map(|pair| {
                    let a = pair[0];
                    let b = *pair.last().unwrap();
                    Extrema {
                        min: if points[a.min].y <= points[b.min].y {
                            a.min
                        } else {
                            b.min
                        },
                        max: if points[a.max].y >= points[b.max].y {
                            a.max
                        } else {
                            b.max
                        },
                    }
                })
                .collect();
            levels.push(next);
        }
        Self {
            points,
            levels,
            ascending,
        }
    }
    fn bounds(&self) -> PlotBounds {
        let first = self.points[0].x;
        let last = self.points.last().unwrap().x;
        let extrema = self.levels.last().unwrap()[0];
        PlotBounds::from_min_max(
            [first.min(last), self.points[extrema.min].y],
            [first.max(last), self.points[extrema.max].y],
        )
    }
    fn interpolate(&self, x: f64) -> Option<PlotPoint> {
        let bounds = self.bounds();
        if !x.is_finite() || x < bounds.min()[0] || x > bounds.max()[0] {
            return None;
        }
        let direction = if self.ascending { 1. } else { -1. };
        let right = self
            .points
            .partition_point(|p| direction * p.x <= direction * x);
        let a = *self.points.get(right.checked_sub(1)?)?;
        // At a repeated X coordinate, choose the last sample in acquisition
        // order; never divide by zero across a vertical discontinuity.
        if a.x == x {
            return Some(a);
        }
        let b = *self.points.get(right)?;
        let t = (x - a.x) / (b.x - a.x);
        let y = a.y * (1. - t) + b.y * t;
        y.is_finite().then_some(PlotPoint::new(x, y))
    }
    fn sample(&self, view: View, output: &mut Vec<PlotPoint>) -> bool {
        output.clear();
        let bounds = self.bounds();
        if bounds.max()[0] < view.min || bounds.min()[0] > view.max {
            return true;
        }
        let direction = if self.ascending { 1. } else { -1. };
        let (min, max) = if self.ascending {
            (view.min, view.max)
        } else {
            (-view.max, -view.min)
        };
        // Retain neighboring samples so lines entering the viewport are intact.
        let start = self
            .points
            .partition_point(|p| direction * p.x < min)
            .saturating_sub(1);
        let end =
            (self.points.partition_point(|p| direction * p.x <= max) + 1).min(self.points.len());
        self.visit(self.levels.len() - 1, 0, &(start..end), view, output);
        output.len() >= end - start
    }
    fn emit(&self, start: usize, end: usize, extrema: Extrema, output: &mut Vec<PlotPoint>) {
        let mut indices = [start, extrema.min, extrema.max, end - 1];
        indices.sort_unstable();
        let mut previous = None;
        for index in indices {
            if previous != Some(index) {
                output.push(self.points[index]);
            }
            previous = Some(index);
        }
    }
    fn visit(
        &self,
        level: usize,
        node: usize,
        visible: &Range<usize>,
        view: View,
        output: &mut Vec<PlotPoint>,
    ) {
        let size = LEAF << level;
        let start = node * size;
        let end = (start + size).min(self.points.len());
        if start >= visible.end || end <= visible.start {
            return;
        }
        if start >= visible.start
            && end <= visible.end
            && (self.points[end - 1].x - self.points[start].x).abs() <= view.step()
        {
            self.emit(start, end, self.levels[level][node], output);
        } else if level > 0 {
            self.visit(level - 1, node * 2, visible, view, output);
            if node * 2 + 1 < self.levels[level - 1].len() {
                self.visit(level - 1, node * 2 + 1, visible, view, output);
            }
        } else {
            // Small boundary leaves are grouped by pixel, preserving both ends
            // and extrema even for highly irregular ngspice time steps.
            let mut start = start.max(visible.start);
            let end = end.min(visible.end);
            while start < end {
                let pixel = ((self.points[start].x - view.min) / view.step()).floor();
                let mut next = start + 1;
                let mut extrema = Extrema {
                    min: start,
                    max: start,
                };
                while next < end
                    && ((self.points[next].x - view.min) / view.step()).floor() == pixel
                {
                    if self.points[next].y < self.points[extrema.min].y {
                        extrema.min = next;
                    }
                    if self.points[next].y > self.points[extrema.max].y {
                        extrema.max = next;
                    }
                    next += 1;
                }
                self.emit(start, next, extrema, output);
                start = next;
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub min: f64,
    pub max: f64,
    pub columns: usize,
}
impl View {
    fn step(self) -> f64 {
        let step = (self.max - self.min) / self.columns.max(1) as f64;
        if step.is_finite() && step > 0. {
            step
        } else {
            f64::MIN_POSITIVE
        }
    }
}

pub struct Series {
    runs: Vec<Run>,
    pub bounds: PlotBounds,
    last_view: Option<View>,
    pub rendered: Vec<Vec<PlotPoint>>,
    pub rendered_full_resolution: bool,
}
impl Series {
    /// Piecewise-linear readout from full-resolution data, independent of LOD.
    /// Gaps are never bridged. For overlapping sweeps, pick the closest Y value.
    pub fn interpolate(&self, cursor: PlotPoint) -> Option<PlotPoint> {
        self.runs
            .iter()
            .filter_map(|run| run.interpolate(cursor.x))
            .min_by(|a, b| (a.y - cursor.y).abs().total_cmp(&(b.y - cursor.y).abs()))
    }
    pub fn new(segments: Vec<Vec<PlotPoint>>) -> Self {
        let mut runs = Vec::new();
        for points in segments.into_iter().filter(|p| !p.is_empty()) {
            let mut ranges = Vec::new();
            let mut start = 0;
            let mut direction = 0.;
            for i in 1..points.len() {
                let delta = points[i].x - points[i - 1].x;
                if delta == 0. {
                    continue;
                }
                if direction != 0. && delta.signum() != direction {
                    ranges.push(start..i);
                    start = i - 1; // Shared endpoint preserves the original path.
                }
                direction = delta.signum();
            }
            ranges.push(start..points.len());
            if ranges.len() == 1 {
                runs.push(Run::new(points));
            } else {
                for range in ranges {
                    runs.push(Run::new(points[range].to_vec()));
                }
            }
        }
        let mut bounds = PlotBounds::NOTHING;
        for run in &runs {
            bounds.merge(&run.bounds());
        }
        Self {
            rendered: vec![Vec::new(); runs.len()],
            rendered_full_resolution: false,
            runs,
            bounds,
            last_view: None,
        }
    }
    pub fn sampled(&self, view: View) -> Vec<Vec<PlotPoint>> {
        self.runs
            .iter()
            .map(|run| {
                let mut output = Vec::new();
                run.sample(view, &mut output);
                output
            })
            .collect()
    }
    /// Cached across hover and Y-only changes. Zooming in restores raw samples.
    pub fn prepare(&mut self, view: View) {
        if self.last_view == Some(view) {
            return;
        }
        self.rendered_full_resolution = true;
        for (run, output) in self.runs.iter().zip(&mut self.rendered) {
            self.rendered_full_resolution &= run.sample(view, output);
        }
        self.last_view = Some(view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linear_interpolation_uses_original_samples_without_bridging_gaps() {
        let mut series = Series::new(vec![
            vec![
                PlotPoint::new(0., 0.),
                PlotPoint::new(2., 8.),
                PlotPoint::new(4., 0.),
            ],
            vec![PlotPoint::new(10., 20.), PlotPoint::new(8., 10.)],
        ]);
        series.prepare(View {
            min: 0.,
            max: 10.,
            columns: 1,
        });
        assert_eq!(
            series.interpolate(PlotPoint::new(1., 0.)),
            Some(PlotPoint::new(1., 4.))
        );
        assert_eq!(
            series.interpolate(PlotPoint::new(3., 0.)),
            Some(PlotPoint::new(3., 4.))
        );
        assert_eq!(
            series.interpolate(PlotPoint::new(9., 0.)),
            Some(PlotPoint::new(9., 15.))
        );
        assert_eq!(series.interpolate(PlotPoint::new(6., 0.)), None);
        assert_eq!(series.interpolate(PlotPoint::new(-1., 0.)), None);
        let series = Series::new(vec![vec![
            PlotPoint::new(0., 0.),
            PlotPoint::new(1., 0.),
            PlotPoint::new(1., 5.),
            PlotPoint::new(2., 7.),
        ]]);
        assert_eq!(
            series.interpolate(PlotPoint::new(1., 0.)),
            Some(PlotPoint::new(1., 5.))
        );
        assert_eq!(
            series.interpolate(PlotPoint::new(1.5, 0.)),
            Some(PlotPoint::new(1.5, 6.))
        );
    }
    #[test]
    fn overview_retains_single_sample_spikes_and_zoom_restores_every_sample() {
        let mut points: Vec<_> = (0..1_000_000)
            .map(|i| PlotPoint::new(i as f64, 0.))
            .collect();
        points[123457].y = 100.;
        points[123459].y = -80.;
        let expected = points[123449..=123471].to_vec();
        let mut series = Series::new(vec![points]);
        series.prepare(View {
            min: 0.,
            max: 999999.,
            columns: 1000,
        });
        let reduced = &series.rendered[0];
        assert!(reduced.len() < 10_000, "{} points", reduced.len());
        assert!(reduced.contains(&PlotPoint::new(123457., 100.)));
        assert!(reduced.contains(&PlotPoint::new(123459., -80.)));
        assert!(reduced.windows(2).all(|p| p[0].x <= p[1].x));
        series.prepare(View {
            min: 123450.,
            max: 123470.,
            columns: 1000,
        });
        assert_eq!(series.rendered[0], expected);
    }
    #[test]
    fn gaps_descending_sweeps_duplicate_x_and_irregular_samples_keep_their_shape() {
        let mut series = Series::new(vec![
            vec![
                PlotPoint::new(4., 0.),
                PlotPoint::new(3., 10.),
                PlotPoint::new(3., -10.),
                PlotPoint::new(0., 2.),
            ],
            vec![PlotPoint::new(10., 8.)],
        ]);
        series.prepare(View {
            min: 0.,
            max: 10.,
            columns: 1000,
        });
        assert_eq!(series.rendered[0], series.runs[0].points);
        assert_eq!(series.rendered[1], [PlotPoint::new(10., 8.)]);
        let points = vec![
            PlotPoint::new(0., 0.),
            PlotPoint::new(1e-9, 12.),
            PlotPoint::new(2e-9, -12.),
            PlotPoint::new(1., 2.),
        ];
        let mut series = Series::new(vec![points.clone()]);
        series.prepare(View {
            min: 0.,
            max: 1.,
            columns: 100,
        });
        assert_eq!(series.rendered[0], points);
    }
    #[test]
    fn reversing_scales_are_split_without_dropping_edges_and_bounds_are_global() {
        let original = vec![
            PlotPoint::new(0., -20.),
            PlotPoint::new(1., 5.),
            PlotPoint::new(2., 10.),
            PlotPoint::new(0., 30.),
        ];
        let mut series = Series::new(vec![original.clone()]);
        assert_eq!(series.runs.len(), 2);
        assert_eq!(series.bounds.min(), [0., -20.]);
        assert_eq!(series.bounds.max(), [2., 30.]);
        series.prepare(View {
            min: 0.,
            max: 2.,
            columns: 100,
        });
        assert_eq!(series.rendered[0], original[..3]);
        assert_eq!(series.rendered[1], original[2..]);
    }

    #[test]
    fn clipping_keeps_offscreen_neighbors_in_both_sweep_directions() {
        let mut sparse = Series::new(vec![vec![
            PlotPoint::new(0., 0.),
            PlotPoint::new(100., 10.),
        ]]);
        sparse.prepare(View {
            min: 49.,
            max: 51.,
            columns: 1000,
        });
        assert_eq!(sparse.rendered[0], sparse.runs[0].points);
        let points: Vec<_> = (0..10_000)
            .rev()
            .map(|i| PlotPoint::new(i as f64, i as f64))
            .collect();
        let mut descending = Series::new(vec![points]);
        descending.prepare(View {
            min: 100.,
            max: 110.,
            columns: 1000,
        });
        assert_eq!(
            descending.rendered[0],
            (99..=111)
                .rev()
                .map(|i| PlotPoint::new(i as f64, i as f64))
                .collect::<Vec<_>>()
        );
    }
}
