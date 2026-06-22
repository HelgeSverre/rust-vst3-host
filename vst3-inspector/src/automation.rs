//! A small parameter-automation demo: drive one parameter from a looping curve while the
//! plugin plays, exercising the library's `ParameterAutomation` / `set_parameter` path.
//!
//! The inspector plays through an `AudioHandle` where the audio callback owns `process_audio`,
//! so the control thread can't see block boundaries — sample-accurate per-block scheduling
//! isn't reachable here. We instead drive the value at UI cadence (~60 fps), which is the
//! honest, realistic approach for a host control thread.

use std::time::Instant;
use vst3_host::parameters::{AutomationCurve, ParameterAutomation};

/// The automation curve shape the demo offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// 0 → 1 ramp that snaps back each period.
    Ramp,
    /// 0 → 1 → 0 triangle.
    Triangle,
    /// Smooth sine LFO between 0 and 1.
    Sine,
}

impl Shape {
    /// All shapes, for a UI selector.
    pub const ALL: [Shape; 3] = [Shape::Ramp, Shape::Triangle, Shape::Sine];

    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            Shape::Ramp => "Ramp",
            Shape::Triangle => "Triangle",
            Shape::Sine => "Sine",
        }
    }
}

/// Build a looping [`ParameterAutomation`] of `shape` over `period_secs`.
pub fn build_curve(shape: Shape, period_secs: f64) -> ParameterAutomation {
    let period = period_secs.max(0.01); // avoid a zero-length (non-looping) curve
    match shape {
        Shape::Ramp => ParameterAutomation::new()
            .add_point(0.0, 0.0)
            .add_point(period, 1.0)
            .with_curve(AutomationCurve::Linear)
            .with_loop(true),
        Shape::Triangle => ParameterAutomation::new()
            .add_point(0.0, 0.0)
            .add_point(period / 2.0, 1.0)
            .add_point(period, 0.0)
            .with_curve(AutomationCurve::Linear)
            .with_loop(true),
        Shape::Sine => {
            // Sample the sine into points; linear interpolation between them approximates it.
            const STEPS: usize = 32;
            let mut curve = ParameterAutomation::new();
            for i in 0..=STEPS {
                let frac = i as f64 / STEPS as f64;
                let t = frac * period;
                let v = 0.5 - 0.5 * (frac * std::f64::consts::TAU).cos(); // 0 at ends, 1 at middle
                curve = curve.add_point(t, v);
            }
            curve.with_curve(AutomationCurve::Linear).with_loop(true)
        }
    }
}

/// UI + runtime state for the automation demo.
#[derive(Debug, Clone)]
pub struct AutomationState {
    /// Whether automation is actively driving the parameter.
    pub enabled: bool,
    /// The parameter being automated (by id).
    pub param_id: Option<u32>,
    /// The curve shape.
    pub shape: Shape,
    /// Loop period in seconds.
    pub period_secs: f64,
    /// When automation was enabled (the curve's time origin).
    pub started: Instant,
    /// Last value written (for display).
    pub last_value: f64,
}

impl AutomationState {
    /// Create the default (disabled) automation state.
    pub fn new() -> Self {
        Self {
            enabled: false,
            param_id: None,
            shape: Shape::Sine,
            period_secs: 2.0,
            started: Instant::now(),
            last_value: 0.0,
        }
    }

    /// The value the curve should hold at `now`, or `None` if disabled / no parameter chosen.
    pub fn value_now(&self, now: Instant) -> Option<f64> {
        if !self.enabled || self.param_id.is_none() {
            return None;
        }
        let elapsed = now.saturating_duration_since(self.started).as_secs_f64();
        build_curve(self.shape, self.period_secs).value_at_time(elapsed)
    }
}

impl Default for AutomationState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramp_spans_zero_to_one() {
        let c = build_curve(Shape::Ramp, 2.0);
        assert!(c.value_at_time(0.0).unwrap().abs() < 1e-9);
        // Near the end of the period the ramp is close to 1.0.
        assert!(c.value_at_time(1.9).unwrap() > 0.9);
    }

    #[test]
    fn triangle_peaks_at_midpoint() {
        let c = build_curve(Shape::Triangle, 2.0);
        assert!(c.value_at_time(1.0).unwrap() > 0.99); // peak at period/2
        assert!(c.value_at_time(0.0).unwrap().abs() < 1e-9);
    }

    #[test]
    fn sine_is_periodic_and_in_range() {
        let c = build_curve(Shape::Sine, 2.0);
        for &t in &[0.0_f64, 0.3, 0.7, 1.1, 1.9] {
            let v = c.value_at_time(t).unwrap();
            assert!((0.0..=1.0).contains(&v), "value {v} out of range at {t}");
            // Looping: t and t+period must agree.
            let w = c.value_at_time(t + 2.0).unwrap();
            assert!((v - w).abs() < 1e-9, "not periodic at {t}: {v} vs {w}");
        }
        // Sine reaches its peak around the midpoint.
        assert!(c.value_at_time(1.0).unwrap() > 0.95);
    }

    #[test]
    fn value_now_none_when_disabled() {
        let mut s = AutomationState::new();
        s.param_id = Some(0);
        assert_eq!(s.value_now(Instant::now()), None); // disabled
        s.enabled = true;
        s.param_id = None;
        assert_eq!(s.value_now(Instant::now()), None); // no param
    }
}
