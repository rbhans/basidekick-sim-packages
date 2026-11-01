//! What the app's trend charts draw. Every controller gets a few ready-made
//! charts (a reading, its setpoints and the output driving it), sampled
//! every couple of seconds for the last hour and every minute for the last
//! day.

use bask_points::{PointDb, PointRef, Units};
use chrono::NaiveDateTime;
use std::collections::VecDeque;

/// How a line reads on a chart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A measurement: solid.
    Reading,
    /// What the loop aims for: dashed.
    Setpoint,
    /// A 0–100 % command, drawn in a strip under the chart.
    Output,
}

struct ChartDef {
    title: &'static str,
    /// The chart shows on controllers that have this point.
    requires: &'static str,
    lines: &'static [(&'static str, Role)],
}

use Role::*;

const CHARTS: &[ChartDef] = &[
    ChartDef {
        title: "Zone temperature",
        requires: "EFF-CLG-SP",
        lines: &[
            ("ZN-T", Reading),
            ("EFF-CLG-SP", Setpoint),
            ("EFF-HTG-SP", Setpoint),
            ("CLG-LOOP", Output),
            ("HTG-LOOP", Output),
            ("CLG-DMD", Output),
            ("HTG-DMD", Output),
        ],
    },
    ChartDef {
        title: "Space temperature",
        requires: "UH-C",
        lines: &[
            ("ZN-T", Reading),
            ("OCC-HTG-SP", Setpoint),
            ("UNOCC-HTG-SP", Setpoint),
        ],
    },
    ChartDef {
        title: "Airflow",
        requires: "AIRFLOW",
        lines: &[
            ("AIRFLOW", Reading),
            ("AIRFLOW-SP", Setpoint),
            ("EFF-MIN-FLOW", Setpoint),
            ("DMPR-O", Output),
        ],
    },
    ChartDef {
        title: "Supply air",
        requires: "SA-T-SP",
        lines: &[
            ("SA-T", Reading),
            ("SA-T-SP", Setpoint),
            ("CLG-VLV", Output),
            ("HTG-VLV", Output),
        ],
    },
    ChartDef {
        title: "Duct static",
        requires: "DSP",
        lines: &[("DSP", Reading), ("DSP-SP", Setpoint), ("SF-SPD", Output)],
    },
    ChartDef {
        title: "Mixed air",
        requires: "SAT-LOOP",
        lines: &[
            ("MA-T", Reading),
            ("RA-T", Reading),
            ("OA-T", Reading),
            ("OAD-O", Output),
        ],
    },
    ChartDef {
        title: "Air temperatures",
        requires: "CMP-STARTS",
        lines: &[
            ("SA-T", Reading),
            ("MA-T", Reading),
            ("RA-T", Reading),
            ("OA-T", Reading),
            ("OAD-O", Output),
        ],
    },
    ChartDef {
        title: "CO2",
        requires: "CO2-SP",
        lines: &[
            ("ZN-CO2", Reading),
            ("CO2-SP", Setpoint),
            ("EFF-MIN-OA", Output),
        ],
    },
    ChartDef {
        title: "Humidity",
        requires: "ZN-RH",
        lines: &[("ZN-RH", Reading)],
    },
    ChartDef {
        title: "Humidity",
        requires: "RA-RH",
        lines: &[("RA-RH", Reading)],
    },
    ChartDef {
        title: "Chilled water",
        requires: "CHWS-T",
        lines: &[
            ("CHWS-T", Reading),
            ("CHWR-T", Reading),
            ("CHWS-SP", Setpoint),
            ("CH-LOAD", Output),
        ],
    },
    ChartDef {
        title: "Hot water",
        requires: "HWS-T",
        lines: &[
            ("HWS-T", Reading),
            ("HWR-T", Reading),
            ("HWS-SP", Setpoint),
            ("B-FIRE", Output),
        ],
    },
    ChartDef {
        title: "Outside air",
        requires: "OA-DP",
        lines: &[("OA-T", Reading), ("OA-DP", Reading)],
    },
    ChartDef {
        title: "Power",
        requires: "PEAK-KW",
        lines: &[
            ("KW", Reading),
            ("HVAC-KW", Reading),
            ("LTG-KW", Reading),
            ("PLUG-KW", Reading),
        ],
    },
];

/// How far back a chart looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Span {
    Minutes15,
    #[default]
    Hour,
    Day,
}

impl Span {
    pub const ALL: [Span; 3] = [Span::Minutes15, Span::Hour, Span::Day];

    pub fn label(self) -> &'static str {
        match self {
            Span::Minutes15 => "15 min",
            Span::Hour => "1 h",
            Span::Day => "24 h",
        }
    }

    pub fn seconds(self) -> f64 {
        match self {
            Span::Minutes15 => 900.0,
            Span::Hour => 3600.0,
            Span::Day => 86_400.0,
        }
    }
}

/// One chart, ready to draw.
#[derive(Clone, Debug)]
pub struct Trend {
    pub title: String,
    /// Units of the readings and setpoints, for the axis.
    pub units: Units,
    pub lines: Vec<TrendLine>,
}

#[derive(Clone, Debug)]
pub struct TrendLine {
    pub name: String,
    pub role: Role,
    /// (seconds before now, as a negative number; value).
    pub points: Vec<(f32, f32)>,
}

/// Samples taken together, at one interval.
struct Buffer {
    every_s: i64,
    len: usize,
    times: VecDeque<NaiveDateTime>,
    values: Vec<VecDeque<f32>>,
}

impl Buffer {
    fn new(every_s: i64, len: usize, series: usize) -> Self {
        Buffer {
            every_s,
            len,
            times: VecDeque::with_capacity(len),
            values: (0..series).map(|_| VecDeque::with_capacity(len)).collect(),
        }
    }

    fn due(&self, now: NaiveDateTime) -> bool {
        self.times
            .back()
            .is_none_or(|t| (now - *t).num_seconds() >= self.every_s)
    }

    fn push(&mut self, now: NaiveDateTime, values: impl Iterator<Item = f32>) {
        if self.times.len() == self.len {
            self.times.pop_front();
            self.values.iter_mut().for_each(|v| {
                v.pop_front();
            });
        }
        self.times.push_back(now);
        for (buf, v) in self.values.iter_mut().zip(values) {
            buf.push_back(v);
        }
    }
}

struct Chart {
    title: &'static str,
    units: Units,
    /// (series index, point name, role)
    lines: Vec<(usize, &'static str, Role)>,
}

pub struct History {
    series: Vec<PointRef>,
    /// Charts per device.
    charts: Vec<Vec<Chart>>,
    fine: Buffer,
    coarse: Buffer,
}

impl History {
    /// Works out every controller's charts from the points it has.
    pub fn new(db: &PointDb) -> Self {
        let mut series: Vec<PointRef> = Vec::new();
        let mut charts = Vec::with_capacity(db.devices.len());
        for (d, dev) in db.devices.iter().enumerate() {
            let at = |name: &str| {
                dev.find_by_name(name).map(|o| PointRef {
                    device: d as u16,
                    object: o as u16,
                })
            };
            let mut mine = Vec::new();
            for def in CHARTS.iter().filter(|c| at(c.requires).is_some()) {
                let mut lines = Vec::new();
                let mut units = Units::NoUnits;
                for &(name, role) in def.lines {
                    let Some(r) = at(name) else { continue };
                    let o = db.obj(r);
                    // only percent commands go in the output strip
                    let role = if role == Output && o.units != Units::Percent {
                        Setpoint
                    } else {
                        role
                    };
                    if role != Output && units == Units::NoUnits {
                        units = o.units;
                    }
                    let i = series.iter().position(|s| *s == r).unwrap_or_else(|| {
                        series.push(r);
                        series.len() - 1
                    });
                    lines.push((i, name, role));
                }
                mine.push(Chart {
                    title: def.title,
                    units,
                    lines,
                });
            }
            charts.push(mine);
        }
        let n = series.len();
        History {
            series,
            charts,
            fine: Buffer::new(2, 1800, n),
            coarse: Buffer::new(60, 1440, n),
        }
    }

    /// Records what the points read now, when a sample is due.
    pub fn sample(&mut self, db: &PointDb, now: NaiveDateTime) {
        for buf in [&mut self.fine, &mut self.coarse] {
            if buf.due(now) {
                let series = &self.series;
                buf.push(
                    now,
                    series
                        .iter()
                        .map(|r| db.obj(*r).present_value.as_f64() as f32),
                );
            }
        }
    }

    /// Forget everything (the clock jumped back, or a new building).
    pub fn clear(&mut self) {
        let n = self.series.len();
        self.fine = Buffer::new(2, 1800, n);
        self.coarse = Buffer::new(60, 1440, n);
    }

    /// The titles of a device's charts, in order.
    pub fn titles(&self, device: usize) -> Vec<&'static str> {
        self.charts
            .get(device)
            .map(|c| c.iter().map(|c| c.title).collect())
            .unwrap_or_default()
    }

    /// One of a device's charts over `span`, ending at `now`.
    pub fn trend(
        &self,
        device: usize,
        chart: usize,
        span: Span,
        now: NaiveDateTime,
    ) -> Option<Trend> {
        let c = self.charts.get(device)?.get(chart)?;
        let buf = if span == Span::Day {
            &self.coarse
        } else {
            &self.fine
        };
        let from = buf
            .times
            .iter()
            .position(|t| (now - *t).num_seconds() as f64 <= span.seconds())
            .unwrap_or(buf.times.len());
        let ago: Vec<f32> = buf
            .times
            .range(from..)
            .map(|t| -((now - *t).num_milliseconds() as f32 / 1000.0))
            .collect();
        Some(Trend {
            title: c.title.into(),
            units: c.units,
            lines: c
                .lines
                .iter()
                .map(|&(i, name, role)| TrendLine {
                    name: name.into(),
                    role,
                    points: ago
                        .iter()
                        .copied()
                        .zip(buf.values[i].range(from..).copied())
                        .collect(),
                })
                .collect(),
        })
    }
}
