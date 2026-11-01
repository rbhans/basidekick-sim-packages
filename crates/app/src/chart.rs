//! Trend charts: a controller's readings and setpoints over time, with the
//! outputs driving them in a 0–100 % strip underneath, the way a BAS trend
//! view shows a loop.

use crate::brand::{self, tint};
use bask_points::Units;
use bask_sim::{Role, Span, Trend, TrendLine};
use gpui_kit::{
    Bounds, Div, FontWeight, Hsla, PathBuilder, Pixels, canvas, div, point, prelude::*, px,
};

/// More than this many samples per line just costs drawing time.
const MAX_POINTS: usize = 480;
const GUTTER: f32 = 46.;

struct Series {
    points: Vec<(f32, f32)>,
    color: Hsla,
    dashed: bool,
}

/// What color a line draws in: cooling blue, heating orange, the hi-vis
/// accent for the main reading.
fn color(line: &TrendLine, nth_reading: usize) -> Hsla {
    let n = line.name.as_str();
    match line.role {
        Role::Setpoint | Role::Output if n.contains("CLG") => brand::cold(),
        Role::Setpoint | Role::Output if n.contains("HTG") || n == "B-FIRE" => brand::warm(),
        Role::Setpoint => brand::ink_2(),
        Role::Output => brand::accent(),
        Role::Reading => match nth_reading {
            0 => brand::accent(),
            1 => brand::ink_2(),
            2 => brand::cold(),
            _ => brand::fault(),
        },
    }
}

fn thin(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let step = points.len().div_ceil(MAX_POINTS).max(1);
    let mut out: Vec<(f32, f32)> = points.iter().step_by(step).copied().collect();
    if let Some(last) = points.last()
        && out.last() != Some(last)
    {
        out.push(*last);
    }
    out
}

/// The smallest y range worth showing, so a steady reading doesn't turn
/// its noise into mountains.
fn min_range(units: Units) -> f32 {
    match units {
        Units::DegreesFahrenheit | Units::DeltaDegreesFahrenheit => 2.0,
        Units::InchesOfWater => 0.2,
        Units::CubicFeetPerMinute => 100.0,
        Units::PartsPerMillion => 100.0,
        Units::PercentRelativeHumidity | Units::Percent => 10.0,
        Units::Kilowatts => 2.0,
        _ => 1.0,
    }
}

fn number(v: f32, units: Units) -> String {
    let d = units.decimals();
    format!("{v:.d$}")
}

fn plot(series: Vec<Series>, span: f32, lo: f32, hi: f32, height: f32) -> Div {
    div().flex_1().h(px(height)).child(
        canvas(
            |_, _, _| (),
            move |b: Bounds<Pixels>, _, window, _| {
                let x =
                    |ago: f32| b.origin.x + b.size.width * ((ago + span) / span).clamp(0.0, 1.0);
                let y = |v: f32| {
                    b.origin.y + b.size.height * (1.0 - ((v - lo) / (hi - lo)).clamp(-0.02, 1.02))
                };
                // grid at the top, middle and bottom
                for f in [0.0, 0.5, 1.0] {
                    let gy = b.origin.y + b.size.height * f;
                    let mut grid = PathBuilder::stroke(px(1.));
                    grid.move_to(point(b.origin.x, gy));
                    grid.line_to(point(b.origin.x + b.size.width, gy));
                    if let Ok(path) = grid.build() {
                        window.paint_path(path, brand::line());
                    }
                }
                for s in &series {
                    if s.points.len() < 2 {
                        continue;
                    }
                    let mut path = PathBuilder::stroke(px(if s.dashed { 1.2 } else { 1.6 }));
                    if s.dashed {
                        path = path.dash_array(&[px(5.), px(4.)]);
                    }
                    let (a, v) = s.points[0];
                    path.move_to(point(x(a), y(v)));
                    for &(a, v) in &s.points[1..] {
                        path.line_to(point(x(a), y(v)));
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, s.color);
                    }
                }
            },
        )
        .size_full(),
    )
}

fn axis(top: String, mid: String, bottom: String, height: f32) -> Div {
    div()
        .flex_none()
        .w(px(GUTTER))
        .h(px(height))
        .flex()
        .flex_col()
        .justify_between()
        .items_end()
        .pr_2()
        .font_family(brand::MONO_FONT)
        .text_xs()
        .text_color(brand::ink_3())
        .child(top)
        .child(mid)
        .child(bottom)
}

/// A trend chart `height` px tall (plus the output strip and legend).
pub fn trend_chart(t: &Trend, span: Span, height: f32) -> Div {
    let seconds = span.seconds() as f32;
    let mut main = Vec::new();
    let mut strip = Vec::new();
    let mut legend = Vec::new();
    let mut readings = 0;
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for line in &t.lines {
        let c = color(line, readings);
        if line.role == Role::Reading {
            readings += 1;
        }
        let pts = thin(&line.points);
        let last = line.points.last().map(|p| p.1);
        let shown = match (line.role, last) {
            (Role::Output, Some(v)) => format!("{v:.0} %"),
            (_, Some(v)) => format!("{} {}", number(v, t.units), t.units.symbol()),
            (_, None) => "–".into(),
        };
        legend.push(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .w(px(12.))
                        .h(px(if line.role == Role::Setpoint { 1. } else { 2. }))
                        .bg(c),
                )
                .child(div().text_color(brand::ink_3()).child(line.name.clone()))
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(brand::ink_2())
                        .child(shown),
                ),
        );
        let s = Series {
            points: pts,
            color: c,
            dashed: line.role == Role::Setpoint,
        };
        if line.role == Role::Output {
            strip.push(s);
        } else {
            for &(_, v) in &s.points {
                lo = lo.min(v);
                hi = hi.max(v);
            }
            main.push(s);
        }
    }
    if lo > hi {
        (lo, hi) = (0.0, 1.0);
    }
    let pad = ((hi - lo) * 0.1).max((min_range(t.units) - (hi - lo)).max(0.0) / 2.0);
    let (lo, hi) = (lo - pad, hi + pad);
    let mid = (lo + hi) / 2.0;
    let has_strip = !strip.is_empty();
    let ago = |f: f32| match span {
        Span::Day => format!("-{:.0} h", seconds * f / 3600.0),
        _ => format!("-{:.0} min", seconds * f / 60.0),
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_x_3()
                .gap_y_1()
                .pl(px(GUTTER))
                .font_family(brand::MONO_FONT)
                .text_xs()
                .children(legend),
        )
        .child(
            div()
                .flex()
                .child(axis(
                    number(hi, t.units),
                    number(mid, t.units),
                    number(lo, t.units),
                    height,
                ))
                .child(plot(main, seconds, lo, hi, height)),
        )
        .when(has_strip, |d| {
            d.child(
                div()
                    .flex()
                    .child(axis("100 %".into(), String::new(), "0".into(), 44.))
                    .child(
                        div()
                            .flex_1()
                            .rounded_sm()
                            .bg(tint(brand::well(), 0.6))
                            .child(plot(strip, seconds, 0.0, 100.0, 44.)),
                    ),
            )
        })
        .child(
            div()
                .flex()
                .justify_between()
                .pl(px(GUTTER))
                .font_family(brand::MONO_FONT)
                .text_xs()
                .text_color(brand::ink_3())
                .child(ago(1.0))
                .child(ago(0.5))
                .child("now"),
        )
}
