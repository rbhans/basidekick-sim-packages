//! Small DDC building blocks: a PI loop whose gains can be changed while it
//! runs, a stage sequencer with the minimum on/off times real compressors
//! and burners need, and an actuator that strokes at a real speed.

/// Proportional-integral loop with output clamping and anti-windup.
/// A positive error drives the output up.
#[derive(Clone, Debug)]
pub struct Pi {
    pub kp: f64,
    /// Integral gain per second.
    pub ki: f64,
    pub min: f64,
    pub max: f64,
    integral: f64,
}

impl Pi {
    pub fn new(kp: f64, integral_time_s: f64, min: f64, max: f64) -> Self {
        Pi {
            kp,
            ki: kp / integral_time_s,
            min,
            max,
            integral: 0.0,
        }
    }

    pub fn update(&mut self, error: f64, dt: f64) -> f64 {
        let p = self.kp * error;
        let integral = (self.integral + self.ki * error * dt).clamp(self.min, self.max);
        // don't integrate further into a saturated output
        let u = p + integral;
        if !((u > self.max && error > 0.0) || (u < self.min && error < 0.0)) {
            self.integral = integral;
        }
        (p + self.integral).clamp(self.min, self.max)
    }

    /// New gains, keeping the integral (so a change bumps the output the
    /// way it would in a real controller). An integral time of 0 makes it
    /// proportional-only.
    pub fn tune(&mut self, kp: f64, integral_time_s: f64) {
        self.kp = kp;
        self.ki = if integral_time_s > 0.0 {
            kp / integral_time_s
        } else {
            0.0
        };
    }

    pub fn reset(&mut self) {
        self.integral = 0.0;
    }

    /// Start from a known output (bumpless transfer).
    pub fn preset(&mut self, output: f64) {
        self.integral = output.clamp(self.min, self.max);
    }
}

/// Turns a 0..100 % demand into stages, with hysteresis and minimum
/// on / off times per stage.
#[derive(Clone, Debug)]
pub struct Stager {
    stages: usize,
    on: Vec<bool>,
    since: Vec<f64>,
    pub min_on_s: f64,
    pub min_off_s: f64,
    /// Stage starts since the sim began, all stages together.
    pub starts: u64,
}

impl Stager {
    pub fn new(stages: u8, min_on_s: f64, min_off_s: f64) -> Self {
        let n = stages.max(1) as usize;
        Stager {
            stages: n,
            on: vec![false; n],
            since: vec![f64::MAX / 2.0; n],
            min_on_s,
            min_off_s,
            starts: 0,
        }
    }

    pub fn stages(&self) -> usize {
        self.stages
    }

    pub fn on(&self, stage: usize) -> bool {
        self.on.get(stage).copied().unwrap_or(false)
    }

    pub fn count(&self) -> usize {
        self.on.iter().filter(|s| **s).count()
    }

    pub fn update(&mut self, demand: f64, enabled: bool, dt: f64) {
        for s in self.since.iter_mut() {
            *s += dt;
        }
        let n = self.stages as f64;
        for i in 0..self.stages {
            // stage i comes on at its share of demand, drops 15 % below it
            let on_at = 100.0 * (i as f64 + 0.3) / n;
            let off_at = (on_at - 15.0).max(2.0);
            let want = enabled
                && if self.on[i] {
                    demand > off_at
                } else {
                    demand > on_at
                };
            // stages come on in order, a minute apart, and leave in reverse
            let allowed = if want {
                i == 0 || (self.on[i - 1] && self.since[i - 1] >= 60.0)
            } else {
                i + 1 >= self.stages || !self.on[i + 1]
            };
            if want != self.on[i] && allowed {
                let held = if self.on[i] {
                    self.min_on_s
                } else {
                    self.min_off_s
                };
                if self.since[i] >= held || !enabled {
                    self.on[i] = want;
                    self.since[i] = 0.0;
                    self.starts += want as u64;
                }
            }
        }
    }

    pub fn all_off(&mut self) {
        for (on, since) in self.on.iter_mut().zip(self.since.iter_mut()) {
            if *on {
                *on = false;
                *since = 0.0;
            }
        }
    }
}

/// A valve or damper actuator: it heads for its command at a fixed speed,
/// taking `stroke_s` to go fully closed to fully open.
#[derive(Clone, Debug)]
pub struct Stroke {
    pub position: f64,
    stroke_s: f64,
}

impl Stroke {
    pub fn new(stroke_s: f64) -> Self {
        Stroke {
            position: 0.0,
            stroke_s,
        }
    }

    pub fn update(&mut self, command: f64, dt: f64) -> f64 {
        let step = 100.0 / self.stroke_s * dt;
        self.position += (command.clamp(0.0, 100.0) - self.position).clamp(-step, step);
        self.position
    }
}

/// Transport delay: hands back what went in `delay_s` ago (air travelling
/// from the coil to the sensor, say). Dead time is what lets a loop with too
/// much gain hunt instead of just overshooting.
#[derive(Clone, Debug)]
pub struct DeadTime {
    delay_s: f64,
    clock: f64,
    buf: std::collections::VecDeque<(f64, f64)>,
}

impl DeadTime {
    pub fn new(delay_s: f64) -> Self {
        DeadTime {
            delay_s,
            clock: 0.0,
            buf: Default::default(),
        }
    }

    pub fn update(&mut self, value: f64, dt: f64) -> f64 {
        self.clock += dt;
        self.buf.push_back((self.clock, value));
        while self.buf.len() > 1 && self.buf[1].0 <= self.clock - self.delay_s {
            self.buf.pop_front();
        }
        self.buf.front().map_or(value, |(_, v)| *v)
    }
}

/// First-order lag toward a target.
pub fn lag(value: f64, target: f64, tau_s: f64, dt: f64) -> f64 {
    value + (target - value) * (1.0 - (-dt / tau_s.max(1e-3)).exp())
}

/// A condition that has to hold for a while before it counts.
#[derive(Clone, Debug, Default)]
pub struct Delay {
    held: f64,
}

impl Delay {
    pub fn update(&mut self, condition: bool, after_s: f64, dt: f64) -> bool {
        self.held = if condition { self.held + dt } else { 0.0 };
        self.held >= after_s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_settles_and_clamps() {
        let mut pi = Pi::new(10.0, 60.0, 0.0, 100.0);
        assert_eq!(pi.update(50.0, 1.0), 100.0);
        // a long saturation doesn't wind the integral past the clamp
        for _ in 0..1000 {
            pi.update(50.0, 1.0);
        }
        assert!(pi.update(-1.0, 1.0) < 100.0);
    }

    #[test]
    fn actuator_strokes_at_its_speed() {
        let mut a = Stroke::new(90.0);
        for _ in 0..45 {
            a.update(100.0, 1.0);
        }
        assert!(
            (a.position - 50.0).abs() < 1e-9,
            "half open after half the stroke"
        );
    }

    #[test]
    fn dead_time_delays() {
        let mut d = DeadTime::new(5.0);
        let out: Vec<f64> = (0..10).map(|i| d.update(i as f64, 1.0)).collect();
        assert_eq!(out[9], 4.0, "what went in five seconds ago");
    }

    #[test]
    fn retuning_keeps_the_integral() {
        let mut pi = Pi::new(10.0, 60.0, 0.0, 100.0);
        for _ in 0..30 {
            pi.update(1.0, 1.0);
        }
        let before = pi.update(0.0, 1.0);
        pi.tune(20.0, 120.0);
        assert_eq!(pi.update(0.0, 1.0), before, "no error, no bump");
    }

    #[test]
    fn stager_respects_order_and_timers() {
        let mut s = Stager::new(2, 180.0, 300.0);
        s.update(100.0, true, 1.0);
        assert_eq!(s.count(), 1, "second stage waits for the first");
        for _ in 0..60 {
            s.update(100.0, true, 1.0);
        }
        assert_eq!(s.count(), 2);
        s.update(0.0, true, 1.0);
        assert_eq!(s.count(), 2, "minimum on time holds both");
        for _ in 0..200 {
            s.update(0.0, true, 1.0);
        }
        assert_eq!(s.count(), 0);
    }
}
