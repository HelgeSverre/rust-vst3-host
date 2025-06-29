use vst3_host::audio::*;

#[test]
fn test_audio_buffers_creation() {
    let buffers = AudioBuffers::new(2, 2, 512, 44100.0);

    assert_eq!(buffers.input_channels(), 2);
    assert_eq!(buffers.output_channels(), 2);
    assert_eq!(buffers.block_size, 512);
    assert_eq!(buffers.sample_rate, 44100.0);

    // Check that buffers are initialized to zero
    for input in &buffers.inputs {
        assert!(input.iter().all(|&x| x == 0.0));
    }
    for output in &buffers.outputs {
        assert!(output.iter().all(|&x| x == 0.0));
    }
}

#[test]
fn test_audio_buffers_clear() {
    let mut buffers = AudioBuffers::new(1, 1, 128, 48000.0);

    // Fill with non-zero values
    buffers.inputs[0].fill(1.0);
    buffers.outputs[0].fill(0.5);

    // Clear
    buffers.clear();

    // Check all are zero
    assert!(buffers.inputs[0].iter().all(|&x| x == 0.0));
    assert!(buffers.outputs[0].iter().all(|&x| x == 0.0));
}

#[test]
fn test_channel_level_db_conversion() {
    let mut level = ChannelLevel::default();

    // Test silence
    level.peak = 0.0;
    assert_eq!(level.peak_db(), f32::NEG_INFINITY);

    // Test 0 dB
    level.peak = 1.0;
    assert!((level.peak_db() - 0.0).abs() < 0.001);

    // Test -6 dB (half amplitude)
    level.peak = 0.5;
    assert!((level.peak_db() - (-6.02)).abs() < 0.1);

    // Test clipping
    level.peak = 2.0;
    assert!(level.is_clipping());
    assert!(level.peak_db() > 0.0);

    level.peak = 0.9;
    assert!(!level.is_clipping());
}

#[test]
fn test_audio_levels_update() {
    let mut levels = AudioLevels::new(2);
    assert_eq!(levels.channels.len(), 2);

    // Create test buffers
    let buffers = vec![
        vec![0.5, -0.3, 0.8, -0.9, 0.2],
        vec![0.1, -0.2, 0.3, -0.4, 0.5],
    ];

    levels.update_from_buffers(&buffers);

    // Check channel 0
    assert_eq!(levels.channels[0].peak, 0.9); // max absolute value
    let expected_rms_0 =
        ((0.5 * 0.5 + 0.3 * 0.3 + 0.8 * 0.8 + 0.9 * 0.9 + 0.2 * 0.2) / 5.0f32).sqrt();
    assert!((levels.channels[0].rms - expected_rms_0).abs() < 0.001);

    // Check channel 1
    assert_eq!(levels.channels[1].peak, 0.5);
    let expected_rms_1 =
        ((0.1 * 0.1 + 0.2 * 0.2 + 0.3 * 0.3 + 0.4 * 0.4 + 0.5 * 0.5) / 5.0f32).sqrt();
    assert!((levels.channels[1].rms - expected_rms_1).abs() < 0.001);
}

#[test]
fn test_audio_levels_clipping_detection() {
    let mut levels = AudioLevels::new(2);

    // No clipping
    let buffers = vec![vec![0.5, -0.8, 0.9], vec![0.3, -0.6, 0.7]];
    levels.update_from_buffers(&buffers);
    assert!(!levels.is_clipping());

    // One channel clipping
    let buffers = vec![vec![0.5, -1.2, 0.9], vec![0.3, -0.6, 0.7]];
    levels.update_from_buffers(&buffers);
    assert!(levels.is_clipping());
}

#[test]
fn test_audio_config_defaults() {
    let config = AudioConfig::default();
    assert_eq!(config.sample_rate, 44100.0);
    assert_eq!(config.block_size, 512);
    assert_eq!(config.input_channels, 0);
    assert_eq!(config.output_channels, 2);
}
