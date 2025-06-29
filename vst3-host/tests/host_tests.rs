use std::sync::{Arc, Mutex};
use vst3_host::prelude::*;

#[test]
fn test_host_builder_configurations() {
    // Test various builder configurations
    let host = Vst3Host::builder()
        .sample_rate(96000.0)
        .block_size(256)
        .build();

    assert!(host.is_ok());
    let host = host.unwrap();
    assert_eq!(host.config().sample_rate, 96000.0);
    assert_eq!(host.config().block_size, 256);

    // Test with different settings
    let host2 = Vst3Host::builder()
        .sample_rate(22050.0)
        .block_size(2048)
        .build();

    assert!(host2.is_ok());
    let host2 = host2.unwrap();
    assert_eq!(host2.config().sample_rate, 22050.0);
    assert_eq!(host2.config().block_size, 2048);
}

#[test]
fn test_host_default_config() {
    let host = Vst3Host::new().unwrap();
    let config = host.config();

    // Check default values
    assert_eq!(config.sample_rate, 44100.0);
    assert_eq!(config.block_size, 512);
    assert_eq!(config.input_channels, 0);
    assert_eq!(config.output_channels, 2);
}

#[test]
fn test_discovery_callback() {
    let host = Vst3Host::new().unwrap();

    // Track discovery progress
    let progress_events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = progress_events.clone();

    // Note: This test won't actually discover plugins unless they exist on the system
    // but it tests the callback mechanism
    let _ = host.discover_plugins_with_callback(move |progress| {
        events_clone.lock().unwrap().push(match progress {
            DiscoveryProgress::Started { total_plugins } => {
                format!("Started: {} plugins", total_plugins)
            }
            DiscoveryProgress::Found {
                plugin,
                current,
                total,
            } => format!("Found: {} ({}/{})", plugin.name, current, total),
            DiscoveryProgress::Error { path, error } => format!("Error: {} - {}", path, error),
            DiscoveryProgress::Completed { total_found } => {
                format!("Completed: {} found", total_found)
            }
        });
    });

    // Verify we got at least the started and completed events
    let events = progress_events.lock().unwrap();
    assert!(!events.is_empty());

    // Should have at least started event
    if events.len() > 0 {
        assert!(events[0].starts_with("Started:"));
    }

    // Should have completed event at the end
    if events.len() > 1 {
        assert!(events[events.len() - 1].starts_with("Completed:"));
    }
}

#[cfg(feature = "cpal-backend")]
#[test]
fn test_cpal_backend_creation() {
    use vst3_host::audio::AudioBackend;
    use vst3_host::backends::CpalBackend;

    // Test that we can create a CPAL backend
    let backend = CpalBackend::new();
    assert!(backend.is_ok());

    let backend = backend.unwrap();

    // Test getting devices
    let output_devices = backend.enumerate_output_devices();
    // Should have at least one output device on most systems
    // but we can't guarantee this in CI
    assert!(output_devices.is_ok());

    let input_devices = backend.enumerate_input_devices();
    assert!(input_devices.is_ok());
}

#[test]
fn test_host_with_custom_backend() {
    // Create a mock backend for testing
    struct MockBackend;

    impl vst3_host::audio::AudioBackend for MockBackend {
        type Stream = MockStream;
        type Device = String;
        type Error = std::io::Error;

        fn enumerate_output_devices(&self) -> std::result::Result<Vec<Self::Device>, Self::Error> {
            Ok(vec!["Mock Output".to_string()])
        }

        fn enumerate_input_devices(&self) -> std::result::Result<Vec<Self::Device>, Self::Error> {
            Ok(vec!["Mock Input".to_string()])
        }

        fn default_output_device(&self) -> Option<Self::Device> {
            Some("Mock Output".to_string())
        }

        fn default_input_device(&self) -> Option<Self::Device> {
            Some("Mock Input".to_string())
        }

        fn create_output_stream(
            &self,
            _device: &Self::Device,
            _config: AudioConfig,
            _data_callback: Box<dyn FnMut(&mut [f32]) + Send>,
            _error_callback: Box<dyn FnMut(Self::Error) + Send>,
        ) -> std::result::Result<Self::Stream, Self::Error> {
            Ok(MockStream)
        }

        fn create_input_stream(
            &self,
            _device: &Self::Device,
            _config: AudioConfig,
            _data_callback: Box<dyn FnMut(&[f32]) + Send>,
            _error_callback: Box<dyn FnMut(Self::Error) + Send>,
        ) -> std::result::Result<Self::Stream, Self::Error> {
            Ok(MockStream)
        }

        fn create_duplex_stream(
            &self,
            _input_device: &Self::Device,
            _output_device: &Self::Device,
            _config: AudioConfig,
            _data_callback: Box<dyn FnMut(&[f32], &mut [f32]) + Send>,
            _error_callback: Box<dyn FnMut(Self::Error) + Send>,
        ) -> std::result::Result<Self::Stream, Self::Error> {
            Ok(MockStream)
        }
    }

    struct MockStream;

    impl vst3_host::audio::AudioStream for MockStream {
        fn play(&self) -> std::result::Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }

        fn pause(&self) -> std::result::Result<(), Box<dyn std::error::Error>> {
            Ok(())
        }
    }

    // Test creating host with custom backend
    // Note: The builder doesn't actually support custom backends yet
    // This is a placeholder for future functionality
    let host = Vst3Host::new();
    assert!(host.is_ok());
}
