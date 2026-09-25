use crate::{
    message::Message,
    theme::{self, Tone},
};
use iced::{
    Element, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse,
    widget::canvas::{self, Canvas, Frame, Geometry, Path, Stroke},
};
use iced_graphics::cache::{Cached, Group};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

const CHART_INSET: f32 = 6.0;
const CHART_GRID_STEPS: u8 = 4;

const UNBOUNDED: f32 = 1.0e7;

/// Works around two iced_tiny_skia 0.13 defects. Its clip mask is built from the frame's own
/// clip placed at the window origin, hiding any path it gets applied to, so an unbounded clip
/// makes the mask fall back to the layer. It also composes geometry as
/// `translate(bounds) * scale(dpi)`, so on HiDPI displays the frame is shifted by the part of
/// the translation that never gets scaled. Zero-height stroke paths are dropped outright, so
/// horizontal rules must be filled rectangles.
fn frame(renderer: &Renderer, bounds: Rectangle, scale: f32) -> Frame {
    let mut frame = Frame::new(renderer, Size::new(UNBOUNDED, UNBOUNDED));
    let missing = (scale - 1.0) / scale.max(f32::EPSILON);
    frame.translate(Vector::new(bounds.x * missing, bounds.y * missing));
    frame
}

struct Entry {
    bounds: Rectangle,
    scale: f32,
    geometry: <Geometry as Cached>::Cache,
}

/// Tessellated chart geometry kept between redraws. Each chart owns a slot; an entry is reused
/// while the chart sits at the same place, and everything is dropped when the data or palette
/// behind it changes. The stock `canvas::Cache` cannot be used because it builds a bounded
/// frame, which trips the clip defect worked around in `frame`.
pub struct Cache {
    group: Group,
    entries: RefCell<HashMap<u64, Entry>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            group: Group::unique(),
            entries: RefCell::new(HashMap::new()),
        }
    }
}

impl Cache {
    pub fn clear(&self) {
        self.entries.borrow_mut().clear();
    }

    fn draw(
        &self,
        slot: u64,
        renderer: &Renderer,
        bounds: Rectangle,
        scale: f32,
        draw: impl FnOnce(&mut Frame),
    ) -> Geometry {
        if let Some(entry) = self.entries.borrow().get(&slot)
            && entry.bounds == bounds
            && entry.scale == scale
        {
            return <Geometry as Cached>::load(&entry.geometry);
        }
        let mut frame = frame(renderer, bounds, scale);
        draw(&mut frame);
        let previous = self
            .entries
            .borrow_mut()
            .remove(&slot)
            .map(|entry| entry.geometry);
        let geometry = frame.into_geometry().cache(self.group, previous);
        let loaded = <Geometry as Cached>::load(&geometry);
        self.entries.borrow_mut().insert(
            slot,
            Entry {
                bounds,
                scale,
                geometry,
            },
        );
        loaded
    }
}

pub fn slot(key: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

/// One sample, positioned by `x` in `0..=1` across the visible window. `managed` is the
/// share of `total` attributable to K3 Up and is layered on top of it.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub x: f32,
    pub total: f32,
    pub managed: f32,
}

pub struct Area<'a> {
    samples: Vec<Sample>,
    max: f32,
    scale: f32,
    cache: &'a Cache,
    slot: u64,
}

impl<'a> Area<'a> {
    pub fn new(samples: Vec<Sample>, max: f32, scale: f32, cache: &'a Cache, slot: u64) -> Self {
        Self {
            samples,
            max: max.max(f32::EPSILON),
            scale,
            cache,
            slot,
        }
    }

    fn plot(size: Size) -> Rectangle {
        Rectangle {
            x: CHART_INSET,
            y: CHART_INSET,
            width: (size.width - CHART_INSET * 2.0).max(1.0),
            height: (size.height - CHART_INSET * 2.0).max(1.0),
        }
    }

    fn project(&self, plot: Rectangle, sample: &Sample, value: f32) -> Point {
        Point::new(
            plot.x + sample.x.clamp(0.0, 1.0) * plot.width,
            plot.y + plot.height * (1.0 - (value / self.max).clamp(0.0, 1.0)),
        )
    }

    fn area(&self, plot: Rectangle, value: impl Fn(&Sample) -> f32) -> Option<Path> {
        let first = self.samples.first()?;
        let last = self.samples.last()?;
        let floor = plot.y + plot.height;
        Some(Path::new(|builder| {
            builder.move_to(Point::new(self.project(plot, first, 0.0).x, floor));
            for sample in &self.samples {
                builder.line_to(self.project(plot, sample, value(sample)));
            }
            builder.line_to(Point::new(self.project(plot, last, 0.0).x, floor));
            builder.close();
        }))
    }

    fn line(&self, plot: Rectangle, value: impl Fn(&Sample) -> f32) -> Option<Path> {
        let first = self.samples.first()?;
        Some(Path::new(|builder| {
            builder.move_to(self.project(plot, first, value(first)));
            for sample in &self.samples {
                builder.line_to(self.project(plot, sample, value(sample)));
            }
        }))
    }

    fn paint(&self, frame: &mut Frame, bounds: Rectangle, palette: theme::ChartPalette) {
        let plot = Self::plot(bounds.size());
        for step in 0..=CHART_GRID_STEPS {
            let y = plot.y + plot.height * f32::from(step) / f32::from(CHART_GRID_STEPS);
            frame.fill_rectangle(
                Point::new(plot.x, y - 0.5),
                Size::new(plot.width, 1.0),
                palette.grid,
            );
        }
        if let Some(path) = self.area(plot, |s| s.total) {
            frame.fill(&path, palette.machine_fill);
        }
        if let Some(path) = self.line(plot, |s| s.total) {
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(palette.machine_line)
                    .with_width(1.5),
            );
        }
        if let Some(path) = self.area(plot, |s| s.managed) {
            frame.fill(&path, palette.managed_fill);
        }
        if let Some(path) = self.line(plot, |s| s.managed) {
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(palette.managed_line)
                    .with_width(1.5),
            );
        }
    }
}

impl canvas::Program<Message> for Area<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let palette = theme::chart(theme);
        vec![
            self.cache
                .draw(self.slot, renderer, bounds, self.scale, |frame| {
                    self.paint(frame, bounds, palette);
                }),
        ]
    }
}

pub fn area<'a>(chart: Area<'a>, height: f32) -> Element<'a, Message> {
    Canvas::new(chart).width(Length::Fill).height(height).into()
}

pub struct Sparkline<'a> {
    values: Vec<f32>,
    max: f32,
    tone: Tone,
    scale: f32,
    cache: &'a Cache,
    slot: u64,
}

impl<'a> Sparkline<'a> {
    pub fn new(
        values: Vec<f32>,
        floor: f32,
        tone: Tone,
        scale: f32,
        cache: &'a Cache,
        slot: u64,
    ) -> Self {
        let peak = values.iter().copied().fold(floor, f32::max);
        Self {
            values,
            max: peak.max(f32::EPSILON),
            tone,
            scale,
            cache,
            slot,
        }
    }

    fn paint(&self, frame: &mut Frame, bounds: Rectangle, palette: theme::SparkPalette) {
        let count = self.values.len();
        if count < 2 {
            return;
        }
        let width = bounds.width;
        let height = bounds.height - 2.0;
        let project = |index: usize, value: f32| {
            Point::new(
                width * index as f32 / (count - 1) as f32,
                1.0 + height * (1.0 - (value / self.max).clamp(0.0, 1.0)),
            )
        };
        let area = Path::new(|builder| {
            builder.move_to(Point::new(0.0, height + 1.0));
            for (index, value) in self.values.iter().enumerate() {
                builder.line_to(project(index, *value));
            }
            builder.line_to(Point::new(width, height + 1.0));
            builder.close();
        });
        let line = Path::new(|builder| {
            builder.move_to(project(0, self.values[0]));
            for (index, value) in self.values.iter().enumerate().skip(1) {
                builder.line_to(project(index, *value));
            }
        });
        frame.fill(&area, palette.fill);
        frame.stroke(
            &line,
            Stroke::default().with_color(palette.line).with_width(1.2),
        );
    }
}

impl canvas::Program<Message> for Sparkline<'_> {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let palette = theme::spark(self.tone)(theme);
        vec![
            self.cache
                .draw(self.slot, renderer, bounds, self.scale, |frame| {
                    self.paint(frame, bounds, palette);
                }),
        ]
    }
}

pub fn sparkline<'a>(spark: Sparkline<'a>, width: f32, height: f32) -> Element<'a, Message> {
    Canvas::new(spark).width(width).height(height).into()
}

pub struct Gauge {
    fraction: f32,
    tone: Tone,
    scale: f32,
}

impl Gauge {
    pub fn new(fraction: f32, tone: Tone, scale: f32) -> Self {
        Self {
            fraction: fraction.clamp(0.0, 1.0),
            tone,
            scale,
        }
    }
}

impl canvas::Program<Message> for Gauge {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let palette = theme::gauge(self.tone)(theme);
        let mut frame = frame(renderer, bounds, self.scale);
        let radius = bounds.height / 2.0;
        let track = Path::rounded_rectangle(Point::ORIGIN, bounds.size(), radius.into());
        frame.fill(&track, palette.track);
        if self.fraction > 0.0 {
            let filled = (bounds.width * self.fraction).max(bounds.height);
            let fill = Path::rounded_rectangle(
                Point::ORIGIN,
                Size::new(filled, bounds.height),
                radius.into(),
            );
            frame.fill(&fill, palette.fill);
        }
        vec![frame.into_geometry()]
    }
}

pub fn gauge<'a>(gauge: Gauge) -> Element<'a, Message> {
    Canvas::new(gauge).width(Length::Fill).height(5).into()
}
