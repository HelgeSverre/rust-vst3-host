//! Parameter types and utilities for VST3 host

use crate::Result;

/// Plugin parameter information
#[derive(Debug, Clone)]
pub struct Parameter {
    /// Parameter ID
    pub id: u32,
    /// Parameter name
    pub name: String,
    /// Current normalized value (0.0 to 1.0)
    pub value: f64,
    /// Minimum value
    pub min: f64,
    /// Maximum value
    pub max: f64,
    /// Default value
    pub default: f64,
    /// Parameter unit (e.g., "Hz", "dB", "%")
    pub unit: String,
    /// Step count (0 = continuous)
    pub step_count: i32,
    /// Whether the parameter can be automated
    pub can_automate: bool,
    /// Whether the parameter is read-only
    pub is_read_only: bool,
    /// Whether the parameter is a bypass control
    pub is_bypass: bool,
    /// Parameter flags
    pub flags: u32,
}

impl Parameter {
    /// Convert normalized value (0.0-1.0) to plain value
    pub fn normalized_to_plain(&self, normalized: f64) -> f64 {
        if self.step_count > 1 {
            // Discrete parameter
            let steps = self.step_count as f64;
            let step = (normalized * steps).round();
            self.min + (step / steps) * (self.max - self.min)
        } else {
            // Continuous parameter
            self.min + normalized * (self.max - self.min)
        }
    }

    /// Convert plain value to normalized value (0.0-1.0)
    pub fn plain_to_normalized(&self, plain: f64) -> f64 {
        if (self.max - self.min).abs() < f64::EPSILON {
            0.0
        } else {
            ((plain - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        }
    }

    /// Format the value as a string with unit
    pub fn format_value(&self, normalized: f64) -> String {
        let plain = self.normalized_to_plain(normalized);

        if self.step_count == 2 {
            // Boolean parameter
            if plain > 0.5 {
                "On".to_string()
            } else {
                "Off".to_string()
            }
        } else if self.step_count > 2 {
            // Discrete parameter
            format!("{:.0} {}", plain, self.unit)
        } else {
            // Continuous parameter
            if self.unit.is_empty() {
                format!("{:.3}", plain)
            } else {
                format!("{:.3} {}", plain, self.unit)
            }
        }
    }

    /// Check if this is a discrete/stepped parameter
    pub fn is_discrete(&self) -> bool {
        self.step_count > 1
    }

    /// Check if this is a boolean/switch parameter
    pub fn is_boolean(&self) -> bool {
        self.step_count == 2
    }
}

/// Parameter change event
#[derive(Debug, Clone)]
pub struct ParameterChange {
    /// Parameter ID
    pub id: u32,
    /// New normalized value (0.0 to 1.0)
    pub value: f64,
    /// Sample offset within the current block
    pub sample_offset: i32,
}

/// Batch parameter update
pub struct ParameterUpdate<'a> {
    updates: Vec<(u32, f64)>,
    plugin: &'a mut crate::Plugin,
}

impl<'a> ParameterUpdate<'a> {
    pub(crate) fn new(plugin: &'a mut crate::Plugin) -> Self {
        Self {
            updates: Vec::new(),
            plugin,
        }
    }

    /// Set a parameter value
    pub fn set(&mut self, id: u32, value: f64) -> &mut Self {
        self.updates.push((id, value));
        self
    }

    /// Apply all parameter updates
    pub fn apply(self) -> Result<()> {
        for (id, value) in self.updates {
            self.plugin.set_parameter(id, value)?;
        }
        Ok(())
    }
}

/// Parameter automation curve types
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AutomationCurve {
    /// Linear interpolation
    Linear,
    /// Exponential curve
    Exponential,
    /// Logarithmic curve
    Logarithmic,
    /// Step (no interpolation)
    Step,
}

/// Parameter automation point
#[derive(Debug, Clone)]
pub struct AutomationPoint {
    /// Time in seconds
    pub time: f64,
    /// Normalized value (0.0 to 1.0)
    pub value: f64,
    /// Curve type to next point
    pub curve: AutomationCurve,
}

/// Parameter automation data
#[derive(Debug, Clone)]
pub struct ParameterAutomation {
    /// Automation points
    pub points: Vec<AutomationPoint>,
    /// Whether to loop the automation
    pub looping: bool,
}

impl ParameterAutomation {
    /// Create new automation
    pub fn new() -> Self {
        Self {
            points: Vec::new(),
            looping: false,
        }
    }

    /// Add an automation point
    pub fn add_point(mut self, time: f64, value: f64) -> Self {
        self.points.push(AutomationPoint {
            time,
            value,
            curve: AutomationCurve::Linear,
        });
        self.points
            .sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap());
        self
    }

    /// Set the curve type
    pub fn with_curve(mut self, curve: AutomationCurve) -> Self {
        for point in &mut self.points {
            point.curve = curve;
        }
        self
    }

    /// Enable looping
    pub fn with_loop(mut self, looping: bool) -> Self {
        self.looping = looping;
        self
    }

    /// Get value at specific time
    pub fn value_at_time(&self, time: f64) -> Option<f64> {
        if self.points.is_empty() {
            return None;
        }

        // Handle looping
        let time = if self.looping && !self.points.is_empty() {
            let duration = self.points.last().unwrap().time;
            if duration > 0.0 {
                time % duration
            } else {
                time
            }
        } else {
            time
        };

        // Find surrounding points
        let mut prev = None;
        let mut next = None;

        for (i, point) in self.points.iter().enumerate() {
            if point.time <= time {
                prev = Some(i);
            } else {
                next = Some(i);
                break;
            }
        }

        match (prev, next) {
            (None, _) => Some(self.points[0].value),
            (Some(i), None) => Some(self.points[i].value),
            (Some(i), Some(j)) => {
                let p1 = &self.points[i];
                let p2 = &self.points[j];

                let t = (time - p1.time) / (p2.time - p1.time);

                let value = match p1.curve {
                    AutomationCurve::Linear => p1.value + (p2.value - p1.value) * t,
                    AutomationCurve::Exponential => p1.value + (p2.value - p1.value) * t * t,
                    AutomationCurve::Logarithmic => p1.value + (p2.value - p1.value) * t.sqrt(),
                    AutomationCurve::Step => p1.value,
                };

                Some(value.clamp(0.0, 1.0))
            }
        }
    }
}

impl Default for ParameterAutomation {
    fn default() -> Self {
        Self::new()
    }
}
