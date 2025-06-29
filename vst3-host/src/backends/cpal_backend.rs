//! CPAL audio backend implementation

use crate::{
    audio::{AudioBuffers, AudioConfig},
    backends::AudioBackend,
    error::{Error, Result},
};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    BufferSize, SampleRate, StreamConfig,
};
use std::sync::{Arc, Mutex};

type ProcessCallback = Box<dyn FnMut(&mut AudioBuffers) -> Result<()> + Send>;

/// CPAL-based audio backend
pub struct CpalBackend {
    device: cpal::Device,
    config: StreamConfig,
    stream: Option<cpal::Stream>,
    audio_config: AudioConfig,
    process_callback: Arc<Mutex<Option<ProcessCallback>>>,
    is_running: bool,
}

impl CpalBackend {
    /// Create a new CPAL backend with default device
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()
            .ok_or_else(|| Error::AudioBackendError("No output device available".to_string()))?;
        
        let config = device.default_output_config()
            .map_err(|e| Error::AudioBackendError(format!("Failed to get default config: {}", e)))?;
        
        let sample_rate = config.sample_rate().0 as f64;
        let channels = config.channels() as usize;
        
        // Create stream config with fixed buffer size
        let stream_config = StreamConfig {
            channels: config.channels(),
            sample_rate: config.sample_rate(),
            buffer_size: BufferSize::Fixed(512),
        };
        
        let audio_config = AudioConfig {
            sample_rate,
            block_size: 512,
            input_channels: 0,
            output_channels: channels,
        };
        
        Ok(Self {
            device,
            config: stream_config,
            stream: None,
            audio_config,
            process_callback: Arc::new(Mutex::new(None)),
            is_running: false,
        })
    }
    
    /// Create with specific device
    pub fn with_device(device: cpal::Device) -> Result<Self> {
        let config = device.default_output_config()
            .map_err(|e| Error::AudioBackendError(format!("Failed to get default config: {}", e)))?;
        
        let sample_rate = config.sample_rate().0 as f64;
        let channels = config.channels() as usize;
        
        let stream_config = StreamConfig {
            channels: config.channels(),
            sample_rate: config.sample_rate(),
            buffer_size: BufferSize::Fixed(512),
        };
        
        let audio_config = AudioConfig {
            sample_rate,
            block_size: 512,
            input_channels: 0,
            output_channels: channels,
        };
        
        Ok(Self {
            device,
            config: stream_config,
            stream: None,
            audio_config,
            process_callback: Arc::new(Mutex::new(None)),
            is_running: false,
        })
    }
    
    /// Set the block size
    pub fn set_block_size(&mut self, block_size: usize) -> Result<()> {
        if self.is_running {
            return Err(Error::AudioBackendError(
                "Cannot change block size while running".to_string()
            ));
        }
        
        self.config.buffer_size = BufferSize::Fixed(block_size as u32);
        self.audio_config.block_size = block_size;
        Ok(())
    }
    
    /// Set the sample rate
    pub fn set_sample_rate(&mut self, sample_rate: f64) -> Result<()> {
        if self.is_running {
            return Err(Error::AudioBackendError(
                "Cannot change sample rate while running".to_string()
            ));
        }
        
        self.config.sample_rate = SampleRate(sample_rate as u32);
        self.audio_config.sample_rate = sample_rate;
        Ok(())
    }
}

impl AudioBackend for CpalBackend {
    fn start(&mut self) -> Result<()> {
        if self.is_running {
            return Ok(());
        }
        
        let callback_arc = Arc::clone(&self.process_callback);
        let channels = self.audio_config.output_channels;
        let block_size = self.audio_config.block_size;
        let sample_rate = self.audio_config.sample_rate;
        
        // Create a buffer to accumulate samples
        let buffer_arc = Arc::new(Mutex::new(vec![0.0f32; channels * block_size]));
        let buffer_position = Arc::new(Mutex::new(0usize));
        
        let stream = self.device.build_output_stream(
            &self.config,
            {
                let buffer_arc = Arc::clone(&buffer_arc);
                let buffer_position_arc = Arc::clone(&buffer_position);
                
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Clear output buffer
                    data.fill(0.0);
                    
                    let mut callback_guard = callback_arc.lock().unwrap();
                    if let Some(ref mut callback) = *callback_guard {
                        let mut buffers = AudioBuffers::new(0, channels, data.len() / channels, sample_rate);
                        
                        // Process audio
                        if let Ok(()) = callback(&mut buffers) {
                            // Copy processed audio to output
                            for (i, sample) in data.iter_mut().enumerate() {
                                let channel = i % channels;
                                let frame = i / channels;
                                if frame < buffers.outputs[channel].len() {
                                    *sample = buffers.outputs[channel][frame];
                                }
                            }
                        }
                    }
                }
            },
            move |err| {
                eprintln!("Audio stream error: {}", err);
            },
            None
        ).map_err(|e| Error::AudioBackendError(format!("Failed to build output stream: {}", e)))?;
        
        stream.play()
            .map_err(|e| Error::AudioBackendError(format!("Failed to start stream: {}", e)))?;
        
        self.stream = Some(stream);
        self.is_running = true;
        Ok(())
    }
    
    fn stop(&mut self) -> Result<()> {
        if let Some(stream) = self.stream.take() {
            drop(stream);
        }
        self.is_running = false;
        Ok(())
    }
    
    fn is_running(&self) -> bool {
        self.is_running
    }
    
    fn set_process_callback<F>(&mut self, callback: F) -> Result<()>
    where
        F: FnMut(&mut AudioBuffers) -> Result<()> + Send + 'static,
    {
        let mut callback_guard = self.process_callback.lock()
            .map_err(|_| Error::AudioBackendError("Failed to lock callback".to_string()))?;
        *callback_guard = Some(Box::new(callback));
        Ok(())
    }
    
    fn sample_rate(&self) -> f64 {
        self.audio_config.sample_rate
    }
    
    fn block_size(&self) -> usize {
        self.audio_config.block_size
    }
}

impl Drop for CpalBackend {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}