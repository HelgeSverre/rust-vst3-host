use vst3_host::parameters::*;

#[test]
fn test_parameter_creation() {
    let param = Parameter {
        id: 100,
        name: "Volume".to_string(),
        unit: "dB".to_string(),
        value: 0.5,
        min: 0.0,
        max: 1.0,
        default: 0.7,
        step_count: 0,
        can_automate: true,
        is_read_only: false,
        is_bypass: false,
        flags: 0,
    };

    assert_eq!(param.id, 100);
    assert_eq!(param.name, "Volume");
    assert_eq!(param.unit, "dB");
    assert_eq!(param.value, 0.5);
    assert_eq!(param.min, 0.0);
    assert_eq!(param.max, 1.0);
    assert_eq!(param.default, 0.7);
    assert_eq!(param.step_count, 0);
    assert!(param.can_automate);
    assert!(!param.is_read_only);
    assert!(!param.is_bypass);
}

#[test]
fn test_parameter_value_validation() {
    let param = Parameter {
        id: 1,
        name: "Test".to_string(),
        unit: String::new(),
        value: 0.5,
        min: 0.0,
        max: 1.0,
        default: 0.5,
        step_count: 0,
        can_automate: true,
        is_read_only: false,
        is_bypass: false,
        flags: 0,
    };

    // Values should be clamped to min/max range
    assert!(param.value >= param.min);
    assert!(param.value <= param.max);
    assert!(param.default >= param.min);
    assert!(param.default <= param.max);
}

#[test]
fn test_parameter_normalization() {
    let param = Parameter {
        id: 1,
        name: "Gain".to_string(),
        unit: "dB".to_string(),
        value: 0.5,
        min: -12.0,
        max: 12.0,
        default: 0.0,
        step_count: 0,
        can_automate: true,
        is_read_only: false,
        is_bypass: false,
        flags: 0,
    };

    // Test normalized to plain
    assert_eq!(param.normalized_to_plain(0.0), -12.0);
    assert_eq!(param.normalized_to_plain(0.5), 0.0);
    assert_eq!(param.normalized_to_plain(1.0), 12.0);

    // Test plain to normalized
    assert_eq!(param.plain_to_normalized(-12.0), 0.0);
    assert_eq!(param.plain_to_normalized(0.0), 0.5);
    assert_eq!(param.plain_to_normalized(12.0), 1.0);
}

#[test]
fn test_parameter_formatting() {
    // Continuous parameter
    let gain = Parameter {
        id: 1,
        name: "Gain".to_string(),
        unit: "dB".to_string(),
        value: 0.5,
        min: -12.0,
        max: 12.0,
        default: 0.0,
        step_count: 0,
        can_automate: true,
        is_read_only: false,
        is_bypass: false,
        flags: 0,
    };

    assert_eq!(gain.format_value(0.5), "0.000 dB");

    // Boolean parameter
    let bypass = Parameter {
        id: 2,
        name: "Bypass".to_string(),
        unit: String::new(),
        value: 0.0,
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step_count: 2,
        can_automate: false,
        is_read_only: false,
        is_bypass: true,
        flags: 0,
    };

    assert_eq!(bypass.format_value(0.0), "Off");
    assert_eq!(bypass.format_value(1.0), "On");
}

#[test]
fn test_parameter_types() {
    // Continuous parameter
    let continuous = Parameter {
        id: 1,
        name: "Volume".to_string(),
        unit: "%".to_string(),
        value: 0.5,
        min: 0.0,
        max: 100.0,
        default: 75.0,
        step_count: 0,
        can_automate: true,
        is_read_only: false,
        is_bypass: false,
        flags: 0,
    };

    assert!(!continuous.is_discrete());
    assert!(!continuous.is_boolean());

    // Discrete parameter
    let discrete = Parameter {
        id: 2,
        name: "Mode".to_string(),
        unit: String::new(),
        value: 0.0,
        min: 0.0,
        max: 3.0,
        default: 0.0,
        step_count: 4,
        can_automate: true,
        is_read_only: false,
        is_bypass: false,
        flags: 0,
    };

    assert!(discrete.is_discrete());
    assert!(!discrete.is_boolean());

    // Boolean parameter
    let boolean = Parameter {
        id: 3,
        name: "Enable".to_string(),
        unit: String::new(),
        value: 1.0,
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step_count: 2,
        can_automate: true,
        is_read_only: false,
        is_bypass: false,
        flags: 0,
    };

    assert!(boolean.is_discrete());
    assert!(boolean.is_boolean());
}

#[test]
fn test_parameter_change() {
    let change = ParameterChange {
        id: 42,
        value: 0.75,
        sample_offset: 128,
    };

    assert_eq!(change.id, 42);
    assert_eq!(change.value, 0.75);
    assert_eq!(change.sample_offset, 128);
}

#[test]
fn test_parameter_automation_basic() {
    let automation = ParameterAutomation::new()
        .add_point(0.0, 0.0)
        .add_point(1.0, 1.0)
        .add_point(2.0, 0.5);

    assert_eq!(automation.points.len(), 3);
    assert!(!automation.looping);

    // Test interpolation
    assert_eq!(automation.value_at_time(0.0), Some(0.0));
    assert_eq!(automation.value_at_time(1.0), Some(1.0));
    assert_eq!(automation.value_at_time(2.0), Some(0.5));

    // Test linear interpolation between points
    let mid_value = automation.value_at_time(0.5).unwrap();
    assert!((mid_value - 0.5).abs() < 0.001);
}

#[test]
fn test_parameter_automation_curves() {
    // Test different curve types
    let linear = ParameterAutomation::new()
        .add_point(0.0, 0.0)
        .add_point(1.0, 1.0)
        .with_curve(AutomationCurve::Linear);

    let exponential = ParameterAutomation::new()
        .add_point(0.0, 0.0)
        .add_point(1.0, 1.0)
        .with_curve(AutomationCurve::Exponential);

    let step = ParameterAutomation::new()
        .add_point(0.0, 0.0)
        .add_point(1.0, 1.0)
        .with_curve(AutomationCurve::Step);

    // Linear should be 0.5 at midpoint
    assert_eq!(linear.value_at_time(0.5), Some(0.5));

    // Exponential should be less than 0.5 at midpoint
    let exp_mid = exponential.value_at_time(0.5).unwrap();
    assert!(exp_mid < 0.5);
    assert!(exp_mid > 0.0);

    // Step should remain at first value
    assert_eq!(step.value_at_time(0.5), Some(0.0));
    assert_eq!(step.value_at_time(0.99), Some(0.0));
}

#[test]
fn test_parameter_automation_looping() {
    let automation = ParameterAutomation::new()
        .add_point(0.0, 0.0)
        .add_point(1.0, 1.0)
        .add_point(2.0, 0.0)
        .with_loop(true);

    assert!(automation.looping);

    // Test looping behavior
    assert_eq!(automation.value_at_time(0.0), Some(0.0));
    assert_eq!(automation.value_at_time(2.0), Some(0.0));
    assert_eq!(automation.value_at_time(4.0), Some(0.0)); // Should loop back
    assert_eq!(automation.value_at_time(3.0), Some(1.0)); // 3.0 % 2.0 = 1.0
}

#[test]
fn test_bypass_parameter() {
    let bypass = Parameter {
        id: 0,
        name: "Bypass".to_string(),
        unit: String::new(),
        value: 0.0,
        min: 0.0,
        max: 1.0,
        default: 0.0,
        step_count: 2,
        can_automate: false,
        is_read_only: false,
        is_bypass: true,
        flags: 0,
    };

    assert!(bypass.is_bypass);
    assert!(!bypass.can_automate);
    assert!(bypass.is_boolean());
    assert_eq!(bypass.format_value(0.0), "Off");
    assert_eq!(bypass.format_value(1.0), "On");
}
