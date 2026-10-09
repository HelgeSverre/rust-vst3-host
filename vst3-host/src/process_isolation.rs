//! Process isolation for VST3 plugin hosting
//!
//! This module provides functionality to run VST3 plugins in separate processes
//! for improved stability and crash protection.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Default time to wait for a helper response before treating the plugin as hung.
pub(crate) const DEFAULT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default deadline for the slow class of commands ([`is_slow_command`]).
///
/// Loading a plugin binary or serializing a large state blob legitimately takes far longer
/// than a process block, so those commands get their own (longer) deadline: the short
/// per-block deadline exists to catch a plugin hung inside `process()`, and applying it to a
/// cold-cache module load would SIGKILL a helper that is merely slow.
pub(crate) const DEFAULT_SLOW_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum bytes accepted for a single line on the protocol stream. Longer lines are
/// discarded rather than buffered, so a helper (or a plugin sharing its stdout) that never
/// terminates a line cannot grow the host's memory without bound.
const MAX_STATE_BASE64_BYTES: usize = crate::plugin::MAX_STATE_SNAPSHOT_BYTES.div_ceil(3) * 4;
const MAX_RESPONSE_LINE_BYTES: usize = MAX_STATE_BASE64_BYTES + 1024 * 1024;

/// Maximum unread lines buffered from the helper. Beyond this the reader drops new lines
/// (counting them) instead of queueing them forever.
const MAX_QUEUED_RESPONSES: usize = 64;

/// Upper bound on an audio channel count taken from the wire.
const MAX_WIRE_CHANNELS: usize = 256;

/// Upper bound on frames-per-channel taken from the wire.
const MAX_WIRE_FRAMES: usize = 1 << 20;

/// Upper bound on a bus count taken from the wire.
const MAX_WIRE_BUSES: i32 = 256;

/// The in-process host has two independently bounded feedback sources: processor output
/// parameters and controller/editor changes. One response drains both, so its wire cap is the
/// sum of their 4096-entry limits.
const MAX_WIRE_PARAMETER_CHANGES: usize = 8192;

/// Whether a command belongs to the slow class — module load and state I/O — which gets
/// [`DEFAULT_SLOW_COMMAND_TIMEOUT`] instead of the per-block response deadline.
pub(crate) fn is_slow_command(command: &HostCommand) -> bool {
    matches!(
        command,
        HostCommand::LoadPlugin { .. }
            | HostCommand::SaveState
            | HostCommand::LoadState { .. }
            | HostCommand::GetProgramData { .. }
            | HostCommand::SetProgramData { .. }
            | HostCommand::GetUnitData { .. }
            | HostCommand::SetUnitData { .. }
    )
}

/// Lossless wire encoding for audio sample buffers.
///
/// JSON cannot represent a non-finite number: `serde_json` writes NaN and ±∞ as `null`, and
/// `null` will not deserialize back into an `f32`. A single non-finite sample from a plugin
/// would therefore break every block on the boundary. Samples cross as base64 of their
/// little-endian IEEE-754 bit patterns instead, which is exact for every `f32` — and about
/// three times smaller than the decimal number array it replaces.
pub(crate) mod audio_codec {
    use super::{Deserialize, Deserializer, Serializer, MAX_WIRE_CHANNELS, MAX_WIRE_FRAMES};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    /// Encode one channel's samples as base64 of their little-endian bit patterns.
    pub(super) fn encode_channel(samples: &[f32]) -> String {
        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            bytes.extend_from_slice(&s.to_bits().to_le_bytes());
        }
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let n = (u32::from(chunk[0]) << 16) | (u32::from(b1) << 8) | u32::from(b2);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        out
    }

    fn sextet(c: u8) -> Option<u32> {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        Some(u32::from(v))
    }

    /// Decode one channel, rejecting anything that isn't well-formed base64 of whole samples.
    pub(super) fn decode_channel(encoded: &str) -> Option<Vec<f32>> {
        let bytes = encoded.as_bytes();
        if bytes.len() % 4 != 0 {
            return None;
        }
        let mut raw = Vec::with_capacity(bytes.len() / 4 * 3);
        for chunk in bytes.chunks(4) {
            let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
            if pad > 2 {
                return None;
            }
            let mut n = 0u32;
            for (i, &c) in chunk.iter().enumerate() {
                if c == b'=' {
                    if i < 4 - pad {
                        return None; // padding is only legal at the end of the group
                    }
                    continue;
                }
                n |= sextet(c)? << (18 - 6 * i);
            }
            raw.push((n >> 16) as u8);
            if pad < 2 {
                raw.push((n >> 8) as u8);
            }
            if pad < 1 {
                raw.push(n as u8);
            }
        }
        if raw.len() % 4 != 0 {
            return None;
        }
        Some(
            raw.chunks_exact(4)
                .map(|b| f32::from_bits(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
                .collect(),
        )
    }

    pub(crate) fn serialize<S: Serializer>(
        channels: &[Vec<f32>],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(channels.iter().map(|c| encode_channel(c)))
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<Vec<f32>>, D::Error> {
        let encoded = Vec::<String>::deserialize(deserializer)?;
        if encoded.len() > MAX_WIRE_CHANNELS {
            log::warn!(
                "isolation: clamping {} wire channels to {MAX_WIRE_CHANNELS}",
                encoded.len()
            );
        }
        encoded
            .iter()
            .take(MAX_WIRE_CHANNELS)
            .map(|c| {
                let mut samples = decode_channel(c).ok_or_else(|| {
                    serde::de::Error::custom("malformed base64 audio channel payload")
                })?;
                if samples.len() > MAX_WIRE_FRAMES {
                    log::warn!(
                        "isolation: clamping {} wire frames to {MAX_WIRE_FRAMES}",
                        samples.len()
                    );
                    samples.truncate(MAX_WIRE_FRAMES);
                }
                Ok(samples)
            })
            .collect()
    }
}

/// Compact, bounded wire encoding for opaque plugin state.
///
/// `Vec<u8>`'s default JSON representation is a decimal integer array and can expand a valid
/// 64 MiB state by roughly four times. Base64 keeps the expansion to 4/3. The deserializer also
/// accepts the old array representation so a new host/helper can finish an in-flight exchange
/// with a peer from an earlier release.
pub(crate) mod state_codec {
    use super::{Deserialize, Deserializer, Serializer, MAX_STATE_BASE64_BYTES};

    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const MAX_STATE_BYTES: usize = crate::plugin::MAX_STATE_SNAPSHOT_BYTES;

    pub(crate) fn serialize<S: Serializer>(state: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        if state.len() > MAX_STATE_BYTES {
            return Err(serde::ser::Error::custom("plugin state exceeds wire limit"));
        }
        let mut out = String::with_capacity(state.len().div_ceil(3) * 4);
        for chunk in state.chunks(3) {
            let b1 = chunk.get(1).copied().unwrap_or(0);
            let b2 = chunk.get(2).copied().unwrap_or(0);
            let n = (u32::from(chunk[0]) << 16) | (u32::from(b1) << 8) | u32::from(b2);
            out.push(ALPHABET[(n >> 18) as usize & 63] as char);
            out.push(ALPHABET[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 {
                ALPHABET[(n >> 6) as usize & 63] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                ALPHABET[n as usize & 63] as char
            } else {
                '='
            });
        }
        serializer.serialize_str(&out)
    }

    fn sextet(byte: u8) -> Option<u32> {
        Some(u32::from(match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        }))
    }

    fn decode(encoded: &str) -> Option<Vec<u8>> {
        let bytes = encoded.as_bytes();
        if bytes.len() > MAX_STATE_BASE64_BYTES || bytes.len() % 4 != 0 {
            return None;
        }
        let mut raw = Vec::with_capacity(bytes.len() / 4 * 3);
        let chunk_count = bytes.len() / 4;
        for (chunk_index, chunk) in bytes.chunks(4).enumerate() {
            let pad = chunk.iter().rev().take_while(|&&byte| byte == b'=').count();
            if pad > 2 || (pad != 0 && chunk_index + 1 != chunk_count) {
                return None;
            }
            let mut n = 0u32;
            for (index, &byte) in chunk.iter().enumerate() {
                if byte == b'=' {
                    if index < 4 - pad {
                        return None;
                    }
                } else {
                    n |= sextet(byte)? << (18 - 6 * index);
                }
            }
            raw.push((n >> 16) as u8);
            if pad < 2 {
                raw.push((n >> 8) as u8);
            }
            if pad == 0 {
                raw.push(n as u8);
            }
        }
        (raw.len() <= MAX_STATE_BYTES).then_some(raw)
    }

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StateWire {
        Base64(String),
        Legacy(Vec<u8>),
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        match StateWire::deserialize(deserializer)? {
            StateWire::Base64(encoded) => decode(&encoded)
                .ok_or_else(|| serde::de::Error::custom("malformed or oversized plugin state")),
            StateWire::Legacy(state) if state.len() <= MAX_STATE_BYTES => Ok(state),
            StateWire::Legacy(_) => {
                Err(serde::de::Error::custom("plugin state exceeds wire limit"))
            }
        }
    }
}

/// Wire encoding for `f64`s that may legitimately be non-finite.
///
/// Same problem as the audio payload: `serde_json` turns NaN/±∞ into `null`, which then fails
/// to deserialize. Finite values stay plain JSON numbers; non-finite ones cross as their
/// standard textual spelling, which `f64::from_str` parses back exactly.
mod lossless_f64 {
    use super::{Deserializer, Serializer};
    use std::fmt;

    pub(super) fn serialize<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
        if value.is_finite() {
            serializer.serialize_f64(*value)
        } else if value.is_nan() {
            serializer.serialize_str("NaN")
        } else if *value > 0.0 {
            serializer.serialize_str("inf")
        } else {
            serializer.serialize_str("-inf")
        }
    }

    struct AnyF64;

    impl serde::de::Visitor<'_> for AnyF64 {
        type Value = f64;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a number or a non-finite float spelled as a string")
        }

        fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<f64, E> {
            Ok(v)
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<f64, E> {
            Ok(v as f64)
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<f64, E> {
            Ok(v as f64)
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<f64, E> {
            v.parse::<f64>()
                .map_err(|_| E::custom(format!("not a float: {v}")))
        }

        // `null` is what an older peer would have written for a non-finite value; treat it as
        // NaN rather than failing the whole exchange.
        fn visit_unit<E: serde::de::Error>(self) -> Result<f64, E> {
            Ok(f64::NAN)
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
        deserializer.deserialize_any(AnyF64)
    }
}

/// Compact, lossless, bounded encoding for processor/controller parameter feedback.
///
/// Values travel as IEEE-754 bits so a misbehaving plugin returning NaN or infinity cannot make
/// serde_json turn the whole helper response into an undecodable `null`.
mod parameter_changes_codec {
    use super::{Deserializer, Serializer, MAX_WIRE_PARAMETER_CHANGES};
    use serde::de::{SeqAccess, Visitor};
    use serde::ser::SerializeSeq;
    use std::fmt;

    pub(super) fn serialize<S: Serializer>(
        changes: &[(u32, f64)],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        if changes.len() > MAX_WIRE_PARAMETER_CHANGES {
            return Err(serde::ser::Error::custom(
                "parameter feedback exceeds wire limit",
            ));
        }
        let mut sequence = serializer.serialize_seq(Some(changes.len()))?;
        for &(id, value) in changes {
            sequence.serialize_element(&(id, value.to_bits()))?;
        }
        sequence.end()
    }

    struct ParameterChangesVisitor;

    impl<'de> Visitor<'de> for ParameterChangesVisitor {
        type Value = Vec<(u32, f64)>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                formatter,
                "at most {MAX_WIRE_PARAMETER_CHANGES} parameter-id/value-bit pairs"
            )
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
            let capacity = sequence
                .size_hint()
                .unwrap_or(0)
                .min(MAX_WIRE_PARAMETER_CHANGES);
            let mut changes = Vec::with_capacity(capacity);
            while let Some((id, bits)) = sequence.next_element::<(u32, u64)>()? {
                if changes.len() >= MAX_WIRE_PARAMETER_CHANGES {
                    return Err(serde::de::Error::custom(
                        "parameter feedback exceeds wire limit",
                    ));
                }
                changes.push((id, f64::from_bits(bits)));
            }
            Ok(changes)
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(u32, f64)>, D::Error> {
        deserializer.deserialize_seq(ParameterChangesVisitor)
    }
}

/// Clamp a wire-provided channel count into `0..=MAX_WIRE_CHANNELS`.
fn clamped_channel_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    let raw = i32::deserialize(deserializer)?;
    Ok(raw.clamp(0, MAX_WIRE_CHANNELS as i32))
}

/// Clamp a wire-provided bus count into `0..=MAX_WIRE_BUSES`.
fn clamped_bus_count<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    let raw = i32::deserialize(deserializer)?;
    Ok(raw.clamp(0, MAX_WIRE_BUSES))
}

/// Commands that can be sent to the isolated plugin process.
///
/// This enum is the single source of truth for the isolation IPC protocol — the
/// helper binary imports it from here rather than redefining it, so the two halves
/// can never drift apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HostCommand {
    /// Query cached MIDI controller assignments.
    MidiControllerAssignments {
        /// Input or output mapping direction.
        direction: crate::audio::BusDirection,
    },
    /// Queue a normalized value for all parameters mapped to a controller.
    SendMidiControllerAt {
        /// Zero-based event bus index.
        bus: i32,
        /// MIDI channel.
        channel: crate::midi::MidiChannel,
        /// MIDI controller address.
        controller: crate::midi::MidiController,
        /// Normalized value in 0..=1.
        value: f64,
        /// Offset within the next audio block.
        sample_offset: i32,
    },
    /// Deliver a live controller notification for MIDI learn.
    NotifyLiveMidiController {
        /// Zero-based event bus index.
        bus: i32,
        /// MIDI channel.
        channel: crate::midi::MidiChannel,
        /// MIDI controller address.
        controller: crate::midi::MidiController,
    },
    /// Queue a live MIDI event and deferred learning.
    SendLiveMidiEvent {
        /// Live input event.
        event: crate::midi::MidiEvent,
    },
    /// Query keyswitch metadata.
    Keyswitches {
        /// Zero-based event bus index.
        bus: i32,
        /// MIDI channel.
        channel: crate::midi::MidiChannel,
    },

    /// Seek the project transport.
    SetTransportPosition {
        /// Position for the next block.
        position: crate::plugin::TransportPosition,
    },
    /// Query the next block's project position.
    TransportPosition,
    /// Load a plugin from the specified path, configured for the given audio settings.
    LoadPlugin {
        /// Path to the `.vst3` bundle.
        path: String,
        /// Sample rate to configure the plugin for.
        #[serde(with = "lossless_f64")]
        sample_rate: f64,
        /// Block size to configure the plugin for.
        block_size: u32,
        /// Transport tempo (BPM) to advertise in the plugin's host `ProcessContext`.
        #[serde(with = "lossless_f64")]
        tempo: f64,
        /// Time signature numerator to advertise in the host `ProcessContext`.
        time_sig_numerator: i32,
        /// Time signature denominator to advertise in the host `ProcessContext`.
        time_sig_denominator: i32,
        /// Specific current or moduleinfo-retired audio class id to instantiate.
        #[serde(default)]
        class_id: Option<String>,
    },
    /// Unload the current plugin
    UnloadPlugin,
    /// Create plugin GUI
    CreateGui,
    /// Close plugin GUI
    CloseGui,
    /// Start the plugin's audio processing.
    StartProcessing,
    /// Stop the plugin's audio processing.
    StopProcessing,
    /// Re-run the plugin's `setupProcessing` at a new sample rate / block size.
    Reconfigure {
        /// New sample rate in Hz.
        #[serde(with = "lossless_f64")]
        sample_rate: f64,
        /// New block size in frames.
        block_size: u32,
    },
    /// Switch the plugin between real-time and offline (`kOffline`) processing.
    SetProcessMode {
        /// Desired process mode.
        mode: crate::plugin::ProcessMode,
    },
    /// Set a parameter (normalized 0.0..=1.0).
    SetParameter {
        /// Parameter id.
        id: u32,
        /// Normalized value.
        #[serde(with = "lossless_f64")]
        value: f64,
    },
    /// Schedule a parameter change at a sample offset within the next process block.
    SetParameterAt {
        /// Parameter id.
        id: u32,
        /// Normalized value.
        #[serde(with = "lossless_f64")]
        value: f64,
        /// Sample offset within the next processed block.
        offset: i32,
    },
    /// Set the transport tempo (BPM) advertised in the plugin's host `ProcessContext`, taking
    /// effect on the next processed block.
    SetTempo {
        /// Transport tempo in beats per minute (validated `> 0` on the host side).
        #[serde(with = "lossless_f64")]
        bpm: f64,
    },
    /// Set the transport time signature advertised in the plugin's host `ProcessContext`,
    /// taking effect on the next processed block.
    SetTimeSignature {
        /// Time signature numerator (validated `> 0` on the host side).
        numerator: i32,
        /// Time signature denominator (validated `1|2|4|8|16` on the host side).
        denominator: i32,
    },
    /// Toggle the transport playing state (`kPlaying`) in the plugin's host `ProcessContext`,
    /// taking effect on the next processed block.
    SetPlaying {
        /// Whether the transport is playing.
        playing: bool,
    },
    /// Read a parameter's current normalized value.
    GetParameter {
        /// Parameter id.
        id: u32,
    },
    /// Read all parameters.
    GetAllParameters,
    /// Ask the plugin to format a normalized value as a display string.
    FormatParameter {
        /// Parameter id.
        id: u32,
        /// Normalized value to format.
        #[serde(with = "lossless_f64")]
        normalized: f64,
    },
    /// Send a MIDI event to the plugin.
    SendMidi {
        /// The event to deliver.
        event: crate::midi::MidiEvent,
    },
    /// Schedule a MIDI event at a sample offset within the next process block.
    SendMidiAt {
        /// The event to deliver.
        event: crate::midi::MidiEvent,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Send a fully owned VST3 event, including pointer-backed SysEx/text payloads.
    SendPluginEvent {
        /// The event to deliver.
        event: crate::midi::PluginEvent,
    },
    /// Release all notes currently tracked by the plugin.
    MidiPanic,
    /// Process one block of audio. `inputs` is per-channel; `frames` is the block length.
    Process {
        /// Per-channel input samples (`[channel][frame]`), carried as base64 bit patterns.
        #[serde(with = "audio_codec")]
        inputs: Vec<Vec<f32>>,
        /// Number of frames in this block.
        frames: u32,
    },
    /// Process one block while preserving every VST3 audio bus.
    ProcessBuses {
        /// Input buses in bus-index order, including inactive buses.
        inputs: Vec<crate::audio::AudioBusBuffer>,
        /// Output bus shapes and activation flags.
        outputs: Vec<crate::audio::AudioBusConfig>,
        /// Number of frames in this block.
        frames: u32,
    },
    /// Query per-bus channel counts and activation state.
    AudioBusLayout,
    /// Serialize the plugin's current state to an opaque byte blob.
    SaveState,
    /// Restore the plugin's state from a blob previously returned by `SaveState`.
    LoadState {
        /// The opaque state bytes.
        #[serde(with = "state_codec")]
        data: Vec<u8>,
        /// Where the bytes came from, so the helper's `setState` stream carries the same
        /// `IStreamAttributes` an in-process restore would.
        ///
        /// Absent on the wire means [`crate::plugin::StateContext::Project`], which is what
        /// every release before this field sent — so a new helper reads an old host's
        /// `LoadState`, and an old helper simply ignores a field it does not know.
        #[serde(default)]
        context: crate::plugin::StateContext,
    },
    /// Start a note (MPE). The helper's plugin allocates the per-voice note id and returns
    /// it in [`HostResponse::NoteStarted`] (in isolation the helper owns the real plugin).
    NoteOn {
        /// MIDI channel, 0-based index (`MidiChannel::as_index`).
        channel: u8,
        /// Note number (0-127).
        note: u8,
        /// Velocity (0-127).
        velocity: u8,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Release a note previously started with [`HostCommand::NoteOn`].
    NoteOff {
        /// Raw note id returned by `NoteOn`.
        note_id: i32,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Send a per-note expression value (normalized 0..1) for a voice. The expression
    /// dimension crosses the boundary as the serializable `NoteExpressionType` enum.
    SendNoteExpression {
        /// Raw note id returned by `NoteOn`.
        note_id: i32,
        /// Which note-expression dimension to set.
        kind: crate::midi::NoteExpressionType,
        /// Normalized expression value (0..1).
        #[serde(with = "lossless_f64")]
        value: f64,
        /// Sample offset within the next processed block.
        sample_offset: i32,
    },
    /// Enumerate the per-note expressions the plugin advertises (`INoteExpressionController`).
    NoteExpressions {
        /// Event bus index.
        bus: i32,
        /// Channel index.
        channel: i16,
    },
    /// Select a program in a unit's program list (`IUnitInfo`).
    SelectProgram {
        /// Unit id (the root unit is `0`).
        unit_id: i32,
        /// 0-based index into the unit's program list.
        program_index: i32,
    },
    /// Activate or deactivate a single bus (`IComponent::activateBus`).
    SetBusActive {
        /// Whether the bus carries audio or events.
        media_type: crate::audio::MediaType,
        /// Whether the bus is an input or an output.
        direction: crate::audio::BusDirection,
        /// 0-based bus index within its `(media_type, direction)` group.
        bus_index: i32,
        /// `true` to activate, `false` to deactivate.
        active: bool,
    },
    /// Query each audio bus's current speaker arrangement (`IAudioProcessor::getBusArrangement`).
    BusArrangements,
    /// Request specific speaker arrangements for the audio buses (re-runs `setupProcessing`).
    SetBusArrangements {
        /// Desired arrangement per input bus, in bus-index order.
        inputs: Vec<crate::audio::SpeakerArrangement>,
        /// Desired arrangement per output bus, in bus-index order.
        outputs: Vec<crate::audio::SpeakerArrangement>,
    },
    /// Enumerate the plugin's units and their program lists (`IUnitInfo`).
    GetUnits,
    /// Query the currently selected unit.
    GetSelectedUnit,
    /// Select a unit.
    SelectUnit {
        /// Unit id.
        unit_id: i32,
    },
    /// Query pitch names for a program.
    ProgramPitchNames {
        /// Program-list id.
        program_list_id: i32,
        /// Program index.
        program_index: i32,
    },
    /// Read opaque data for a program.
    GetProgramData {
        /// Program-list id.
        program_list_id: i32,
        /// Program index.
        program_index: i32,
    },
    /// Restore opaque data for a program.
    SetProgramData {
        /// Program-list id.
        program_list_id: i32,
        /// Program index.
        program_index: i32,
        /// Opaque plugin data.
        #[serde(with = "state_codec")]
        data: Vec<u8>,
    },
    /// Read opaque data for a unit.
    GetUnitData {
        /// Unit id.
        unit_id: i32,
    },
    /// Restore opaque data for a unit.
    SetUnitData {
        /// Unit id.
        unit_id: i32,
        /// Opaque plugin data.
        #[serde(with = "state_codec")]
        data: Vec<u8>,
    },
    /// Begin a host-edit session.
    BeginHostEdit {
        /// Parameter id.
        parameter_id: u32,
    },
    /// End a host-edit session.
    EndHostEdit {
        /// Parameter id.
        parameter_id: u32,
    },
    /// Forward live MIDI controller input to `IMidiLearn`.
    SendMidiLearn {
        /// Event bus.
        bus: i32,
        /// MIDI channel.
        channel: i16,
        /// MIDI controller number.
        controller: u16,
    },
    /// Report the current automation state.
    SetAutomationState {
        /// Automation state.
        state: crate::plugin::AutomationState,
    },
    /// Map a parameter id from a plugin class this controller replaces (`IRemapParamID`).
    RemapParameterId {
        /// Canonical separator-free 32-hex-character VST3 class id.
        old_plugin_uid: String,
        /// Parameter id used by the replaced plugin class.
        old_param_id: u32,
    },
    /// Query the plugin's reported processing latency in samples
    /// (`IAudioProcessor::getLatencySamples`).
    LatencySamples,
    /// Query the plugin's reported tail length in samples (`IAudioProcessor::getTailSamples`).
    TailSamples,
    /// Resolve a MIDI controller to the parameter it's mapped to (`IMidiMapping`).
    MidiCcToParameter {
        /// Event input bus index.
        bus: i32,
        /// 0-based MIDI channel.
        channel: i16,
        /// MIDI controller number (0-127, or a VST3 special such as aftertouch/pitch-bend).
        cc: u16,
    },
    /// Drain the ordered parameter-edit gesture log (begin/change/end) the helper's plugin has
    /// accumulated from its editor since the last poll.
    TakeParameterEdits,
    /// Drain processor- and controller-originated parameter value feedback.
    TakeParameterChanges,
    /// Drain ordered `IComponentHandler2` requests from the helper.
    TakeHostNotifications,
    /// Dispatch and drain owned VST3 data-exchange blocks from the helper.
    TakeDataExchangeBlocks,
    /// Execute an item from a pending plugin-provided context menu.
    ExecuteContextMenuItem {
        /// Host-assigned popup id.
        menu_id: u64,
        /// Host-assigned item id within the popup.
        item_id: u32,
    },
    /// Dismiss a pending plugin-provided context menu.
    DismissContextMenu {
        /// Host-assigned popup id.
        menu_id: u64,
    },
    /// Drain accumulated `restartComponent` flags without applying lifecycle changes.
    TakeRestartFlags,
    /// Drain restart flags and apply required lifecycle changes in the helper.
    ServiceHostRequests,
    /// Shutdown the helper process
    Shutdown,
}

/// Responses from the isolated plugin process
#[derive(Debug, Serialize, Deserialize)]
pub enum HostResponse {
    /// Reply to MidiControllerAssignments.
    MidiControllerAssignments {
        /// Returned assignments.
        assignments: Vec<crate::midi::MidiControllerAssignment>,
    },
    /// Reply to NotifyLiveMidiController.
    NotifyLiveMidiController {
        /// Whether the plugin accepted the learn notification.
        accepted: bool,
    },
    /// Reply to Keyswitches.
    Keyswitches {
        /// Returned keyswitches.
        keyswitches: Vec<crate::midi::KeyswitchInfo>,
    },

    /// Current transport position.
    TransportPosition {
        /// Position for the next block.
        position: crate::plugin::TransportPosition,
    },
    /// Operation succeeded with message
    Success {
        /// Human-readable success detail.
        message: String,
    },
    /// Operation failed with error
    Error {
        /// Error detail.
        message: String,
    },
    /// Plugin crashed
    Crashed {
        /// Crash detail.
        message: String,
    },
    /// Per-channel audio output data (`[channel][frame]`), plus any MIDI the plugin
    /// emitted during the block (arpeggiators, MPE, etc.).
    AudioOutput {
        /// Output samples per channel, carried as base64 bit patterns.
        #[serde(with = "audio_codec")]
        outputs: Vec<Vec<f32>>,
        /// Owned VST3 events the plugin emitted this block, in order.
        output_events: Vec<crate::midi::PluginEvent>,
    },
    /// Bus-preserving audio output from a `ProcessBuses` request.
    BusAudioOutput {
        /// Output buses in VST3 bus-index order.
        outputs: Vec<crate::audio::AudioBusBuffer>,
        /// Owned VST3 events emitted during the block.
        output_events: Vec<crate::midi::PluginEvent>,
    },
    /// Current audio-bus layout and activation state.
    AudioBusLayout {
        /// Complete input/output layout.
        layout: crate::audio::AudioBusLayout,
    },
    /// A single parameter value (normalized).
    ParameterValue {
        /// Normalized value.
        #[serde(with = "lossless_f64")]
        value: f64,
    },
    /// A formatted parameter display string.
    ParameterString {
        /// The plugin-rendered display string.
        value: String,
    },
    /// A list of parameters.
    Parameters {
        /// All parameters reported by the plugin.
        params: Vec<crate::parameters::Parameter>,
    },
    /// Opaque plugin state bytes (reply to `SaveState`).
    State {
        /// The serialized state.
        #[serde(with = "state_codec")]
        data: Vec<u8>,
    },
    /// The isolated editor window was created (reply to `CreateGui`); carries the
    /// plugin-reported editor size so the host can report it without a second round-trip.
    GuiCreated {
        /// Editor width in pixels.
        width: i32,
        /// Editor height in pixels.
        height: i32,
    },
    /// Plugin information
    PluginInfo {
        /// Vendor / manufacturer.
        vendor: String,
        /// Plugin name.
        name: String,
        /// Version string (may be empty if the plugin doesn't report one).
        version: String,
        /// Plugin sub-categories (e.g. "Fx", "Instrument|Synth"); may be empty.
        category: String,
        /// Unique plugin class id (hex).
        uid: String,
        /// Whether the plugin has an editor.
        has_gui: bool,
        /// Audio input bus count (clamped to a sane maximum on receipt).
        #[serde(deserialize_with = "clamped_bus_count")]
        audio_inputs: i32,
        /// Audio output bus count (clamped to a sane maximum on receipt).
        #[serde(deserialize_with = "clamped_bus_count")]
        audio_outputs: i32,
        /// Total output audio channels across all output buses (clamped on receipt: the host
        /// sizes buffers from this number, so it is not trusted verbatim).
        #[serde(deserialize_with = "clamped_channel_count")]
        output_channels: i32,
        /// Whether the plugin has a MIDI/event input bus.
        has_midi_input: bool,
        /// Whether the plugin has a MIDI/event output bus.
        has_midi_output: bool,
        /// Current/retired class-id mappings discovered by the helper.
        #[serde(default)]
        compatibility: Vec<crate::discovery::ClassCompatibility>,
    },
    /// A note was started (reply to `NoteOn`); carries the helper-allocated raw note id.
    NoteStarted {
        /// Raw note id the host wraps back into a `NoteId`.
        note_id: i32,
    },
    /// The per-note expressions the plugin advertises (reply to `NoteExpressions`).
    NoteExpressions {
        /// The advertised note-expression dimensions.
        expressions: Vec<crate::midi::NoteExpressionInfo>,
    },
    /// The ordered parameter-edit gestures drained from the helper (reply to
    /// `TakeParameterEdits`).
    ParameterEdits {
        /// The gesture events, in the order the plugin's editor reported them.
        edits: Vec<crate::plugin::ParameterEdit>,
    },
    /// Processor- and controller-originated parameter feedback drained from the helper.
    ParameterChanges {
        /// Parameter id and normalized value pairs, in drain order.
        #[serde(with = "parameter_changes_codec")]
        changes: Vec<(u32, f64)>,
    },
    /// Ordered `IComponentHandler2` requests drained from the helper.
    HostNotifications {
        /// The queued host requests.
        notifications: Vec<crate::plugin::HostNotification>,
    },
    /// Owned VST3 data-exchange blocks drained from the helper.
    DataExchangeBlocks {
        /// Block snapshots, in delivery order.
        blocks: Vec<crate::plugin::DataExchangeBlock>,
    },
    /// Accumulated `restartComponent` flags.
    RestartFlags {
        /// Raw VST3 restart flag bits.
        bits: i32,
    },
    /// Each audio bus's current speaker arrangement (reply to `BusArrangements`).
    BusArrangements {
        /// The input/output arrangements.
        arrangements: crate::audio::BusArrangements,
    },
    /// The plugin's units and program lists (reply to `GetUnits`).
    Units {
        /// The advertised units.
        units: Vec<crate::plugin::PluginUnit>,
    },
    /// Currently selected unit.
    SelectedUnit {
        /// Unit id, or `None` when units are unsupported/unselected.
        unit_id: Option<i32>,
    },
    /// Program pitch names.
    ProgramPitchNames {
        /// Advertised pitch names.
        names: Vec<crate::plugin::ProgramPitchName>,
    },
    /// Opaque program/unit data.
    OpaqueData {
        /// Whether the corresponding interface/data kind is supported.
        supported: bool,
        /// Opaque bytes; empty is a valid supported payload.
        #[serde(with = "state_codec")]
        data: Vec<u8>,
    },
    /// The plugin's reported processing latency in samples (reply to `LatencySamples`).
    LatencySamples {
        /// Latency in samples.
        samples: u32,
    },
    /// The plugin's reported tail length in samples (reply to `TailSamples`).
    TailSamples {
        /// Tail length in samples.
        samples: u32,
    },
    /// The parameter a MIDI controller is mapped to, if any (reply to `MidiCcToParameter`).
    MidiParameterMapping {
        /// The mapped parameter id, or `None` if unmapped / not implemented.
        id: Option<u32>,
    },
    /// A parameter-id compatibility mapping returned by `IRemapParamID`.
    RemappedParameter {
        /// The replacement parameter id, or `None` if unsupported/unmapped.
        id: Option<u32>,
    },
}

/// The helper process's write half of the request/response protocol.
///
/// A loaded VST3 plugin shares the helper's file descriptors, and third-party plugins do
/// print to stdout. Since the protocol is a line stream over the helper's stdout, one stray
/// `printf` would be read by the host as a response and desynchronise every later exchange.
///
/// [`ProtocolChannel::claim`] therefore takes stdout away from the plugin: on Unix it
/// duplicates the inherited stdout onto a private, close-on-exec descriptor and repoints file
/// descriptor 1 at stderr, so plugin output is merged into the helper's stderr instead. Call
/// it once, before any plugin code can run.
///
/// On non-Unix platforms the protocol still runs over the process stdout; a plugin writing
/// there corrupts the stream (the host drops lines it cannot parse, which limits the damage
/// to noise, but a well-formed line would still be taken for a response).
pub struct ProtocolChannel {
    inner: ProtocolChannelInner,
}

#[cfg(unix)]
type ProtocolChannelInner = std::fs::File;
#[cfg(not(unix))]
type ProtocolChannelInner = std::io::Stdout;

impl ProtocolChannel {
    /// Claim the protocol channel for this process. See the type documentation.
    #[cfg(unix)]
    pub fn claim() -> Self {
        use std::os::fd::FromRawFd;

        // SAFETY: `F_DUPFD_CLOEXEC`/`dup` return a fresh descriptor owned by this process, and
        // `dup2` rebinds STDOUT_FILENO, which this process also owns. Both are the documented
        // POSIX contracts; no descriptor Rust already owns as a `File` is aliased or closed.
        let fd = unsafe {
            let private = match libc::fcntl(libc::STDOUT_FILENO, libc::F_DUPFD_CLOEXEC, 3) {
                fd if fd >= 0 => Some(fd),
                _ => match libc::dup(libc::STDOUT_FILENO) {
                    fd if fd >= 0 => Some(fd),
                    _ => None,
                },
            };
            match private {
                Some(fd) => {
                    // Plugin (and helper) writes to stdout now land on stderr.
                    libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO);
                    fd
                }
                None => {
                    // Nothing to fall back to: keep speaking on fd 1 as-is, so the protocol
                    // still works even though plugin writes can pollute it.
                    eprintln!("helper: could not privatise the protocol channel; plugin writes to stdout may corrupt it");
                    libc::STDOUT_FILENO
                }
            }
        };
        // SAFETY: `fd` is a descriptor this process owns — a fresh duplicate, or stdout
        // itself when duplication failed — and this channel becomes its sole owner.
        Self {
            inner: unsafe { std::fs::File::from_raw_fd(fd) },
        }
    }

    /// Claim the protocol channel for this process. See the type documentation.
    #[cfg(not(unix))]
    pub fn claim() -> Self {
        Self {
            inner: std::io::stdout(),
        }
    }
}

impl Write for ProtocolChannel {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Manages a plugin running in an isolated process.
///
/// Responses are read on a background thread and delivered over a channel, so
/// [`Self::send_command`] can wait with a deadline: a hung plugin yields a timeout
/// error (and the child is killed) instead of blocking the host forever, and a
/// crashed helper surfaces as a disconnect error rather than a silent wedge.
pub struct PluginHostProcess {
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    /// Lines received from the helper's stdout (one JSON response each).
    responses: Receiver<String>,
    /// Background reader thread handle (joined, with a bound, on shutdown).
    reader: Option<JoinHandle<()>>,
    /// Set by the reader thread when it is about to exit, so shutdown can join it only when
    /// the join is known to be instant.
    reader_finished: Arc<AtomicBool>,
    /// Lines queued but not yet taken by [`Self::send_command`]; bounds the reader's buffer.
    queued: Arc<AtomicUsize>,
    /// Lines the reader refused because the queue was full or the line was oversized.
    discarded_by_reader: Arc<AtomicU64>,
    /// Lines received that did not parse as a response (helper noise), for diagnostics.
    unparsed_lines: u64,
    /// How long to wait for a single response before declaring a timeout.
    timeout: Duration,
    /// Deadline for the slow command class ([`is_slow_command`]).
    slow_timeout: Duration,
    /// Set once the child has been killed/exited so we stop trying to talk to it.
    dead: bool,
    /// The helper binary this child was spawned from, kept so [`Self::send_command`] can put a
    /// fresh one in its place when a load kills it. Resolved once by [`Self::new`], so a
    /// respawn cannot pick a different binary than the original search did.
    helper_path: std::path::PathBuf,
}

/// How many times a `LoadPlugin` that killed the helper is replayed against a freshly spawned
/// one.
///
/// Loading a plugin runs the plugin's own module and instance initialization, and some real
/// plugins lose a race in there: Dexed (JUCE) segfaults or aborts inside
/// `juce::MessageQueue::runLoopSourceCallback` on roughly 6% of cold loads, dispatching an
/// async update into an object whose construction has not finished. Nothing the host does
/// provokes it and nothing it does prevents it — but a *fresh* helper is an independent roll,
/// and the crashed one had no state worth preserving (its load never completed), so replaying
/// the load is exactly what a human would do.
///
/// One retry, not a loop: it takes a 6% failure to 0.4%, while a plugin that genuinely cannot
/// load still reports that after two attempts instead of grinding.
const LOAD_CRASH_RETRIES: u32 = 1;

/// Pause before replaying a crashed load, so the retry is not a tight respawn loop and the
/// dying child's teardown (crash reporter, atexit handlers) has a moment to finish.
const LOAD_CRASH_RETRY_BACKOFF: Duration = Duration::from_millis(250);

/// Why one host↔helper exchange failed.
///
/// The variants exist to tell "the helper died while handling *this* command" — the only case
/// worth replaying against a fresh child — from a timeout, a helper that was already gone, or
/// a local transport failure. Each carries the message the public API reports, unchanged:
/// [`crate::internal::isolated_plugin_impl`] classifies those strings into
/// [`crate::Error::PluginCrashed`] / [`crate::Error::PluginTimeout`].
enum ExchangeError {
    /// The helper was already known dead; nothing was sent.
    AlreadyDead(String),
    /// The helper crashed or exited while this command was in flight.
    DiedDuringCommand(String),
    /// No answer within the deadline; the child has been killed.
    TimedOut(String),
    /// The command could not be encoded (never reached the helper).
    Encoding(String),
}

impl From<ExchangeError> for String {
    fn from(error: ExchangeError) -> String {
        match error {
            ExchangeError::AlreadyDead(message)
            | ExchangeError::DiedDuringCommand(message)
            | ExchangeError::TimedOut(message)
            | ExchangeError::Encoding(message) => message,
        }
    }
}

/// One read from the helper's stdout.
enum ReadLine {
    /// A complete line (newline included), within the size cap.
    Line(Vec<u8>),
    /// A line longer than the cap; its bytes were discarded rather than buffered.
    Oversized,
    /// The stream ended (helper exited) or errored.
    Eof,
}

/// Read one newline-terminated line, discarding (rather than buffering) anything longer than
/// `max` bytes so a helper that never terminates a line cannot exhaust host memory.
fn read_bounded_line(reader: &mut impl BufRead, max: usize) -> ReadLine {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let budget = (max + 1 - line.len()) as u64;
        let mut chunk = Vec::new();
        let read = match reader.by_ref().take(budget).read_until(b'\n', &mut chunk) {
            Ok(n) => n,
            Err(_) => return ReadLine::Eof,
        };
        let complete = chunk.last() == Some(&b'\n');
        if !oversized {
            line.extend_from_slice(&chunk);
            if line.len() > max {
                oversized = true;
                line = Vec::new();
            }
        }
        if read == 0 {
            // EOF: a trailing partial line is still worth delivering, an oversized one is not.
            return if oversized {
                ReadLine::Oversized
            } else if line.is_empty() {
                ReadLine::Eof
            } else {
                ReadLine::Line(line)
            };
        }
        if complete {
            return if oversized {
                ReadLine::Oversized
            } else {
                ReadLine::Line(line)
            };
        }
    }
}

/// Search the existing deployment/build locations using native executable filenames.
fn find_helper_binary(exe_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    // Try different possible helper names and locations
    let helper_names = ["vst3-host-helper", "vst3-inspector-helper"]
        .map(|name| format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let mut helper_path = None;

    // First try in the same directory as the executable
    for name in &helper_names {
        let path = exe_dir.join(name);
        if path.exists() {
            helper_path = Some(path);
            break;
        }
    }

    // If not found and we're in an examples directory, try parent
    if helper_path.is_none() && exe_dir.file_name() == Some(std::ffi::OsStr::new("examples")) {
        if let Some(parent_dir) = exe_dir.parent() {
            for name in &helper_names {
                let path = parent_dir.join(name);
                if path.exists() {
                    helper_path = Some(path);
                    break;
                }
            }
        }
    }

    // Also check common cargo target directories.
    //
    // Only when *we* are running from inside a cargo target tree — that is the case this
    // fallback exists for (test binaries live in `target/<profile>/deps`, so the checks above
    // don't find the sibling helper). For a deployed application it would be a liability: the
    // walk reaches into directories an unprivileged process can write, and the binary it finds
    // is spawned and then trusted for every answer the host gets about the plugin. Deployed
    // builds use the explicit `helper_path`/env override or a helper beside the executable.
    if helper_path.is_none() && crate::discovery::running_from_cargo_target(exe_dir) {
        // Try to find the workspace root and look in target/debug or target/release
        let mut current_dir = exe_dir;
        while let Some(parent) = current_dir.parent() {
            let debug_path = parent.join("target").join("debug").join(&helper_names[0]);
            let release_path = parent.join("target").join("release").join(&helper_names[0]);

            if debug_path.exists() {
                helper_path = Some(debug_path);
                break;
            } else if release_path.exists() {
                helper_path = Some(release_path);
                break;
            }

            // Check if we've reached a Cargo.toml (workspace root)
            if parent.join("Cargo.toml").exists() {
                break;
            }
            current_dir = parent;
        }
    }

    helper_path
}

impl PluginHostProcess {
    /// Create a new isolated plugin host process
    pub fn new(
        helper_override: Option<std::path::PathBuf>,
        timeout: Duration,
    ) -> Result<Self, String> {
        // An explicit helper path (builder option or the VST3_HOST_HELPER_PATH env var) wins
        // over the heuristic search below — and a missing one is reported clearly here.
        let override_path = helper_override
            .or_else(|| std::env::var_os("VST3_HOST_HELPER_PATH").map(std::path::PathBuf::from));
        if let Some(p) = override_path {
            if !p.exists() {
                return Err(format!(
                    "Configured helper path does not exist: {}",
                    p.display()
                ));
            }
            return Self::spawn(p, timeout);
        }

        // Get the path to our helper executable
        let exe_path =
            std::env::current_exe().map_err(|e| format!("Failed to get current exe: {}", e))?;

        let exe_dir = exe_path.parent().ok_or("Failed to get exe directory")?;

        let helper_path = find_helper_binary(exe_dir)
            .ok_or_else(|| format!("Helper executable not found. Searched in {:?} and parent directories. Make sure to build with --bins flag.", exe_dir))?;

        Self::spawn(helper_path, timeout)
    }

    /// Spawn the helper at `helper_path` and wire up the response reader thread.
    fn spawn(helper_path: std::path::PathBuf, timeout: Duration) -> Result<Self, String> {
        let mut child = Command::new(&helper_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to spawn helper process: {}", e))?;

        let stdin = child.stdin.take().ok_or("Failed to get stdin")?;
        let stdout = child.stdout.take().ok_or("Failed to get stdout")?;

        // Read responses on a background thread so the caller can apply a deadline.
        // The thread ends (dropping the sender) when stdout hits EOF — i.e. when the
        // helper process exits or crashes — which the receiver sees as Disconnected.
        //
        // Nothing the helper sends is trusted for size: lines are read with a byte cap and
        // the queue of not-yet-consumed lines is bounded, so neither an unterminated line nor
        // a helper that spews between commands can grow the host's memory without bound.
        let (tx, rx) = mpsc::channel::<String>();
        let queued = Arc::new(AtomicUsize::new(0));
        let discarded = Arc::new(AtomicU64::new(0));
        let finished = Arc::new(AtomicBool::new(false));
        let reader = std::thread::spawn({
            let queued = Arc::clone(&queued);
            let discarded = Arc::clone(&discarded);
            let finished = Arc::clone(&finished);
            move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    match read_bounded_line(&mut reader, MAX_RESPONSE_LINE_BYTES) {
                        ReadLine::Eof => break,
                        ReadLine::Oversized => {
                            discarded.fetch_add(1, Ordering::Relaxed);
                        }
                        ReadLine::Line(bytes) => {
                            if queued.load(Ordering::Relaxed) >= MAX_QUEUED_RESPONSES {
                                discarded.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            queued.fetch_add(1, Ordering::Relaxed);
                            // Lossy: a plugin can put arbitrary bytes on the stream, and
                            // mangled noise must not end the reader thread.
                            let line = String::from_utf8_lossy(&bytes).into_owned();
                            if tx.send(line).is_err() {
                                break; // receiver dropped
                            }
                        }
                    }
                }
                finished.store(true, Ordering::Release);
            }
        });

        Ok(Self {
            process: Some(child),
            stdin: Some(stdin),
            responses: rx,
            reader: Some(reader),
            reader_finished: finished,
            queued,
            discarded_by_reader: discarded,
            unparsed_lines: 0,
            timeout,
            slow_timeout: DEFAULT_SLOW_COMMAND_TIMEOUT.max(timeout),
            dead: false,
            helper_path,
        })
    }

    /// Put a freshly spawned helper in place of the current (dead) child, keeping the
    /// deadlines and the diagnostic counters this handle has accumulated.
    fn respawn(&mut self) -> Result<(), String> {
        self.shutdown();
        let slow_timeout = self.slow_timeout;
        let unparsed_lines = self.unparsed_lines;
        let discarded = self.discarded_by_reader.load(Ordering::Relaxed);

        *self = Self::spawn(self.helper_path.clone(), self.timeout)?;

        self.slow_timeout = slow_timeout;
        self.unparsed_lines = unparsed_lines;
        self.discarded_by_reader
            .fetch_add(discarded, Ordering::Relaxed);
        Ok(())
    }

    /// Set how long to wait for a helper response before declaring a timeout.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// Set the deadline used for the slow command class — loading a plugin and saving or
    /// restoring its state. Never shorter than the per-command timeout.
    pub fn set_slow_command_timeout(&mut self, timeout: Duration) {
        self.slow_timeout = timeout;
    }

    /// The deadline to apply to `command`.
    fn timeout_for(&self, command: &HostCommand) -> Duration {
        if is_slow_command(command) {
            self.slow_timeout.max(self.timeout)
        } else {
            self.timeout
        }
    }

    /// Drop lines that arrived before this command was sent: they are stale replies or
    /// unsolicited output, never the answer to what we are about to ask.
    fn drop_stale_lines(&mut self) {
        let mut stale = 0u64;
        while self.responses.try_recv().is_ok() {
            self.queued.fetch_sub(1, Ordering::Relaxed);
            stale += 1;
        }
        if stale > 0 {
            self.unparsed_lines += stale;
            log::warn!("isolation: dropped {stale} unsolicited line(s) from the helper");
        }
    }

    /// The child's exit status if it has already exited, `None` while it is still running.
    fn exit_status(&mut self) -> Option<std::process::ExitStatus> {
        self.process
            .as_mut()
            .and_then(|p| p.try_wait().ok().flatten())
    }

    /// How many lines from the helper were discarded (oversized, over-queued, unparseable or
    /// unsolicited). A non-zero count means the helper is putting non-protocol data on the
    /// stream — typically a plugin writing to stdout on a platform where the protocol channel
    /// cannot be made private.
    pub fn discarded_line_count(&self) -> u64 {
        self.unparsed_lines + self.discarded_by_reader.load(Ordering::Relaxed)
    }

    /// Send a command to the helper process and wait (with a deadline) for a response.
    ///
    /// Returns an error without blocking indefinitely if the plugin hangs (the child
    /// is killed) or the helper has crashed/exited.
    ///
    /// A line that does not parse as a response is either a helper that died mid-write —
    /// reported as a crash — or noise on the stream, which is dropped so the exchange stays
    /// in sync instead of answering every later command with the previous one's reply. The
    /// deadline covers the whole exchange, not each individual line.
    ///
    /// One command heals itself: a [`HostCommand::LoadPlugin`] that *kills* the helper is
    /// replayed once against a freshly spawned one. A helper
    /// that died mid-load holds nothing worth preserving — its load never completed — so the
    /// replay is observationally identical to the caller having spawned the helper a moment
    /// later, and it absorbs the cold-load crashes some real plugins lose a race to. A
    /// timeout is not retried (it already cost a full deadline, and a plugin that hangs while
    /// loading will hang again), and no other command is: those run against a helper holding
    /// live plugin state that a fresh process would not have.
    pub fn send_command(&mut self, command: HostCommand) -> Result<HostResponse, String> {
        if !matches!(command, HostCommand::LoadPlugin { .. }) {
            return self.exchange(command).map_err(String::from);
        }

        let mut retries_left = LOAD_CRASH_RETRIES;
        loop {
            match self.exchange(command.clone()) {
                Ok(response) => return Ok(response),
                Err(ExchangeError::DiedDuringCommand(detail)) if retries_left > 0 => {
                    retries_left -= 1;
                    log::warn!(
                        "isolation: the helper died while loading the plugin ({detail}); \
                         retrying once with a fresh helper"
                    );
                    std::thread::sleep(LOAD_CRASH_RETRY_BACKOFF);
                    if let Err(spawn_error) = self.respawn() {
                        // No fresh helper to retry against — report the crash, not the spawn.
                        log::warn!("isolation: could not respawn the helper: {spawn_error}");
                        return Err(detail);
                    }
                }
                Err(other) => return Err(String::from(other)),
            }
        }
    }

    /// One request/response exchange with the current child, with no recovery.
    fn exchange(&mut self, command: HostCommand) -> Result<HostResponse, ExchangeError> {
        if self.dead {
            return Err(ExchangeError::AlreadyDead(
                "Helper process is no longer running".to_string(),
            ));
        }

        let command_json = serde_json::to_string(&command)
            .map_err(|e| ExchangeError::Encoding(format!("Failed to serialize command: {}", e)))?;

        // Anything already queued predates this command.
        self.drop_stale_lines();

        {
            let Some(stdin) = self.stdin.as_mut() else {
                return Err(ExchangeError::AlreadyDead("No stdin available".to_string()));
            };
            if let Err(e) = writeln!(stdin, "{}", command_json).and_then(|()| stdin.flush()) {
                self.dead = true;
                return Err(ExchangeError::DiedDuringCommand(format!(
                    "Failed to write command (helper gone?): {}",
                    e
                )));
            }
        }

        let timeout = self.timeout_for(&command);
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.responses.recv_timeout(remaining) {
                Ok(line) => {
                    self.queued.fetch_sub(1, Ordering::Relaxed);
                    match serde_json::from_str::<HostResponse>(&line) {
                        Ok(response) => return Ok(response),
                        Err(parse_error) => {
                            // A helper that died mid-write leaves a truncated line behind:
                            // that is a crash, not noise, and must be reported as one.
                            if let Some(status) = self.exit_status() {
                                self.dead = true;
                                return Err(ExchangeError::DiedDuringCommand(format!(
                                    "Helper process crashed: exited with {status} while writing a response ({parse_error})"
                                )));
                            }
                            self.unparsed_lines += 1;
                            log::warn!(
                                "isolation: dropping unparseable line from the helper ({parse_error})"
                            );
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // The plugin is hung. Kill the child so it can't wedge us further.
                    self.dead = true;
                    if let Some(ref mut process) = self.process {
                        let _ = process.kill();
                    }
                    return Err(ExchangeError::TimedOut(format!(
                        "Timed out after {:?} waiting for helper response (plugin may have hung)",
                        timeout
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // Reader thread ended => stdout closed => helper exited/crashed.
                    self.dead = true;
                    let detail = match self.check_process_status() {
                        Err(status) => format!("Helper process crashed: {}", status),
                        Ok(()) => "Helper process exited unexpectedly".to_string(),
                    };
                    return Err(ExchangeError::DiedDuringCommand(detail));
                }
            }
        }
    }

    /// Whether the helper process is still considered alive.
    pub fn is_alive(&self) -> bool {
        !self.dead
    }

    /// OS process id of the running helper, if any. Useful for monitoring — and for tests
    /// that need to simulate a crash by killing the helper.
    pub fn helper_pid(&self) -> Option<u32> {
        self.process.as_ref().map(|c| c.id())
    }

    /// Check if the helper process is still running
    pub fn check_process_status(&mut self) -> Result<(), String> {
        if let Some(ref mut process) = self.process {
            match process.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return Err(format!("Helper process exited with status: {}", status));
                    }
                }
                Ok(None) => {
                    // Still running
                    return Ok(());
                }
                Err(e) => {
                    return Err(format!("Failed to check process status: {}", e));
                }
            }
        }
        Ok(())
    }

    /// Shutdown the helper process
    pub fn shutdown(&mut self) {
        // Best-effort Shutdown command (no response expected — the helper just exits).
        // We do NOT use send_command here: it waits for a reply, and Shutdown has none.
        if !self.dead {
            if let (Some(stdin), Ok(json)) = (
                self.stdin.as_mut(),
                serde_json::to_string(&HostCommand::Shutdown),
            ) {
                let _ = writeln!(stdin, "{}", json);
                let _ = stdin.flush();
            }
        }

        // Dropping stdin gives the helper's read loop EOF, guaranteeing it exits even
        // if it ignored the Shutdown command; that in turn ends the reader thread.
        self.stdin = None;

        if let Some(mut process) = self.process.take() {
            // Bounded wait, then SIGKILL: this runs from Drop, so a wedged helper must not
            // be able to hang the host on exit. Poll for a clean exit up to a deadline, then
            // force-kill (mirrors the kill-on-timeout pattern in send_command).
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                match process.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if std::time::Instant::now() >= deadline => {
                        let _ = process.kill();
                        let _ = process.wait();
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                    Err(_) => {
                        let _ = process.kill();
                        break;
                    }
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            // Join only once the thread has signalled that it is finishing. It blocks in a
            // read on the helper's stdout, and that pipe stays open as long as *any* process
            // holds it — a plugin-spawned grandchild that inherited it, for instance. Since
            // this runs from Drop, waiting on that is not an option: after a short grace
            // period the thread is detached instead. It ends by itself at EOF, and a leaked
            // thread beats a host that can never drop a plugin.
            let deadline = std::time::Instant::now() + Duration::from_millis(250);
            while !self.reader_finished.load(Ordering::Acquire) {
                if std::time::Instant::now() >= deadline {
                    log::debug!("isolation: helper stdout still open, detaching reader thread");
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if self.reader_finished.load(Ordering::Acquire) {
                let _ = reader.join();
            }
        }
        self.dead = true;
    }
}

impl Drop for PluginHostProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Result type for process isolation operations
pub type IsolationResult<T> = std::result::Result<T, IsolationError>;

/// Errors that can occur during process isolation
#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Plugin error
    #[error("Plugin error: {0}")]
    Plugin(String),

    /// Plugin crashed
    #[error("Plugin crashed: {0}")]
    Crashed(String),

    /// Helper process not running
    #[error("Helper process not running")]
    NotRunning,

    /// Unexpected response
    #[error("Unexpected response from helper")]
    UnexpectedResponse,
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use crate::midi::{MidiChannel, MidiEvent};

    #[test]
    fn audio_output_carries_midi_across_the_wire() {
        // The Process response carries emitted MIDI alongside audio; check the variant
        // round-trips through the JSON transport host and helper share.
        let resp = HostResponse::AudioOutput {
            outputs: vec![vec![0.0, 0.5], vec![-0.5, 0.0]],
            output_events: vec![
                MidiEvent::NoteOn {
                    channel: MidiChannel::Ch1,
                    note: 60,
                    velocity: 100,
                }
                .into(),
                MidiEvent::NoteOff {
                    channel: MidiChannel::Ch1,
                    note: 60,
                    velocity: 0,
                }
                .into(),
            ],
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        let back: HostResponse = serde_json::from_str(&json).expect("deserialize");
        match back {
            HostResponse::AudioOutput {
                outputs,
                output_events,
            } => {
                assert_eq!(outputs, vec![vec![0.0, 0.5], vec![-0.5, 0.0]]);
                assert_eq!(output_events.len(), 2);
                assert_eq!(
                    output_events[0].to_midi(),
                    Some(MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 100
                    })
                );
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn state_commands_round_trip_across_the_wire() {
        // SaveState/LoadState/State carry the opaque plugin state blob across isolation.
        let blob: Vec<u8> = vec![0, 1, 2, 250, 255, 42];

        let save = serde_json::to_string(&HostCommand::SaveState).expect("serialize SaveState");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&save).expect("deserialize SaveState"),
            HostCommand::SaveState
        ));

        let load = HostCommand::LoadState {
            data: blob.clone(),
            context: crate::plugin::StateContext::Project,
        };
        let load_json = serde_json::to_string(&load).expect("serialize LoadState");
        assert!(
            load_json.contains("\"data\":\""),
            "state should use compact base64, not a JSON integer array"
        );
        match serde_json::from_str::<HostCommand>(&load_json).expect("deserialize LoadState") {
            HostCommand::LoadState { data, context } => {
                assert_eq!(data, blob);
                assert_eq!(context, crate::plugin::StateContext::Project);
            }
            other => panic!("LoadState round-trip changed the variant: {other:?}"),
        }

        let legacy = r#"{"LoadState":{"data":[0,1,2,250,255,42]}}"#;
        match serde_json::from_str::<HostCommand>(legacy).expect("deserialize legacy LoadState") {
            HostCommand::LoadState { data, context } => {
                assert_eq!(data, blob);
                assert_eq!(context, crate::plugin::StateContext::Project);
            }
            other => panic!("legacy LoadState changed the variant: {other:?}"),
        }

        let state = HostResponse::State { data: blob.clone() };
        let state_json = serde_json::to_string(&state).expect("serialize State");
        match serde_json::from_str::<HostResponse>(&state_json).expect("deserialize State") {
            HostResponse::State { data } => assert_eq!(data, blob),
            other => panic!("State round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn set_parameter_at_round_trips_across_the_wire() {
        // The sample-accurate automation command must survive the JSON transport intact
        // (the offset is carried across the isolation boundary).
        let cmd = HostCommand::SetParameterAt {
            id: 42,
            value: 0.75,
            offset: 256,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SetParameterAt");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetParameterAt") {
            HostCommand::SetParameterAt { id, value, offset } => {
                assert_eq!(id, 42);
                assert_eq!(value, 0.75);
                assert_eq!(offset, 256);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn scheduled_midi_offset_ipc_round_trips() {
        // Sample-accurate MIDI must carry its offset across the isolation boundary, so isolated
        // playback schedules the event in the same block position as the in-process path.
        use crate::midi::{MidiChannel, MidiEvent};
        let cmd = HostCommand::SendMidiAt {
            event: MidiEvent::NoteOn {
                channel: MidiChannel::Ch1,
                note: 60,
                velocity: 100,
            },
            sample_offset: 256,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SendMidiAt");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SendMidiAt") {
            HostCommand::SendMidiAt {
                event,
                sample_offset,
            } => {
                assert_eq!(
                    event,
                    MidiEvent::NoteOn {
                        channel: MidiChannel::Ch1,
                        note: 60,
                        velocity: 100
                    }
                );
                assert_eq!(sample_offset, 256);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn owned_sysex_round_trips_in_commands_and_process_output() {
        let event = crate::midi::PluginEvent::sysex(vec![0xf0, 0x7d, 1, 2, 0xf7]).at(37);
        let command = HostCommand::SendPluginEvent {
            event: event.clone(),
        };
        let json = serde_json::to_string(&command).expect("serialize owned event");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize owned event") {
            HostCommand::SendPluginEvent { event: decoded } => assert_eq!(decoded, event),
            other => panic!("owned event command changed variant: {other:?}"),
        }

        let response = HostResponse::AudioOutput {
            outputs: Vec::new(),
            output_events: vec![event.clone()],
        };
        let json = serde_json::to_string(&response).expect("serialize owned output");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize owned output") {
            HostResponse::AudioOutput { output_events, .. } => {
                assert_eq!(output_events, vec![event])
            }
            other => panic!("owned event response changed variant: {other:?}"),
        }
    }

    #[test]
    fn select_program_round_trips_across_the_wire() {
        // Program selection must survive the JSON transport host and helper share.
        let cmd = HostCommand::SelectProgram {
            unit_id: 0,
            program_index: 17,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SelectProgram");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SelectProgram") {
            HostCommand::SelectProgram {
                unit_id,
                program_index,
            } => {
                assert_eq!(unit_id, 0);
                assert_eq!(program_index, 17);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn transport_commands_round_trip_across_the_wire() {
        // The runtime transport mutations must survive the JSON transport intact so the helper
        // applies the same change the host requested.
        let tempo = HostCommand::SetTempo { bpm: 137.5 };
        let json = serde_json::to_string(&tempo).expect("serialize SetTempo");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetTempo") {
            HostCommand::SetTempo { bpm } => assert_eq!(bpm, 137.5),
            other => panic!("round-trip changed the variant: {other:?}"),
        }

        let ts = HostCommand::SetTimeSignature {
            numerator: 7,
            denominator: 8,
        };
        let json = serde_json::to_string(&ts).expect("serialize SetTimeSignature");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetTimeSignature") {
            HostCommand::SetTimeSignature {
                numerator,
                denominator,
            } => assert_eq!((numerator, denominator), (7, 8)),
            other => panic!("round-trip changed the variant: {other:?}"),
        }

        let playing = HostCommand::SetPlaying { playing: false };
        let json = serde_json::to_string(&playing).expect("serialize SetPlaying");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetPlaying") {
            HostCommand::SetPlaying { playing } => assert!(!playing),
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn set_bus_active_round_trips_across_the_wire() {
        use crate::audio::{BusDirection, MediaType};
        let cmd = HostCommand::SetBusActive {
            media_type: MediaType::Audio,
            direction: BusDirection::Input,
            bus_index: 1,
            active: true,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SetBusActive");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize SetBusActive") {
            HostCommand::SetBusActive {
                media_type,
                direction,
                bus_index,
                active,
            } => {
                assert_eq!(media_type, MediaType::Audio);
                assert_eq!(direction, BusDirection::Input);
                assert_eq!(bus_index, 1);
                assert!(active);
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn bus_arrangements_round_trip_across_the_wire() {
        use crate::audio::{BusArrangements, SpeakerArrangement};

        let cmd = serde_json::to_string(&HostCommand::BusArrangements)
            .expect("serialize BusArrangements");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&cmd).expect("deserialize BusArrangements"),
            HostCommand::BusArrangements
        ));

        let set = HostCommand::SetBusArrangements {
            inputs: vec![],
            outputs: vec![SpeakerArrangement::STEREO],
        };
        let set_json = serde_json::to_string(&set).expect("serialize SetBusArrangements");
        match serde_json::from_str::<HostCommand>(&set_json).expect("deserialize") {
            HostCommand::SetBusArrangements { inputs, outputs } => {
                assert!(inputs.is_empty());
                assert_eq!(outputs, vec![SpeakerArrangement::STEREO]);
            }
            other => panic!("SetBusArrangements round-trip changed the variant: {other:?}"),
        }

        let arrangements = BusArrangements {
            inputs: vec![],
            outputs: vec![SpeakerArrangement::STEREO],
        };
        let resp = HostResponse::BusArrangements {
            arrangements: arrangements.clone(),
        };
        let resp_json = serde_json::to_string(&resp).expect("serialize BusArrangements response");
        match serde_json::from_str::<HostResponse>(&resp_json).expect("deserialize") {
            HostResponse::BusArrangements { arrangements: back } => {
                assert_eq!(back, arrangements);
            }
            other => panic!("BusArrangements response round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn get_units_round_trips_across_the_wire() {
        use crate::plugin::PluginUnit;

        let cmd = serde_json::to_string(&HostCommand::GetUnits).expect("serialize GetUnits");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&cmd).expect("deserialize GetUnits"),
            HostCommand::GetUnits
        ));

        let units = vec![PluginUnit {
            id: 0,
            parent_id: -1,
            name: "Root".to_string(),
            program_list_id: Some(12),
            programs: vec!["Init".to_string(), "Lead".to_string()],
        }];
        let resp = HostResponse::Units {
            units: units.clone(),
        };
        let resp_json = serde_json::to_string(&resp).expect("serialize Units");
        match serde_json::from_str::<HostResponse>(&resp_json).expect("deserialize Units") {
            HostResponse::Units { units: back } => assert_eq!(back, units),
            other => panic!("Units round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn latency_and_tail_round_trip_across_the_wire() {
        let latency_cmd = serde_json::to_string(&HostCommand::LatencySamples).expect("serialize");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&latency_cmd).expect("deserialize"),
            HostCommand::LatencySamples
        ));
        let tail_cmd = serde_json::to_string(&HostCommand::TailSamples).expect("serialize");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&tail_cmd).expect("deserialize"),
            HostCommand::TailSamples
        ));

        let latency_resp = HostResponse::LatencySamples { samples: 128 };
        let json = serde_json::to_string(&latency_resp).expect("serialize");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize") {
            HostResponse::LatencySamples { samples } => assert_eq!(samples, 128),
            other => panic!("LatencySamples round-trip changed the variant: {other:?}"),
        }

        let tail_resp = HostResponse::TailSamples { samples: 44100 };
        let json = serde_json::to_string(&tail_resp).expect("serialize");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize") {
            HostResponse::TailSamples { samples } => assert_eq!(samples, 44100),
            other => panic!("TailSamples round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn midi_cc_to_parameter_round_trips_across_the_wire() {
        let cmd = HostCommand::MidiCcToParameter {
            bus: 0,
            channel: 1,
            cc: 74,
        };
        let json = serde_json::to_string(&cmd).expect("serialize MidiCcToParameter");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize") {
            HostCommand::MidiCcToParameter { bus, channel, cc } => {
                assert_eq!((bus, channel, cc), (0, 1, 74));
            }
            other => panic!("MidiCcToParameter round-trip changed the variant: {other:?}"),
        }

        let resp = HostResponse::MidiParameterMapping { id: Some(42) };
        let json = serde_json::to_string(&resp).expect("serialize MidiParameterMapping");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize") {
            HostResponse::MidiParameterMapping { id } => assert_eq!(id, Some(42)),
            other => panic!("MidiParameterMapping round-trip changed the variant: {other:?}"),
        }

        let none_resp = HostResponse::MidiParameterMapping { id: None };
        let json = serde_json::to_string(&none_resp).expect("serialize");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize") {
            HostResponse::MidiParameterMapping { id } => assert_eq!(id, None),
            other => {
                panic!("MidiParameterMapping (None) round-trip changed the variant: {other:?}")
            }
        }
    }

    #[test]
    fn parameter_id_remapping_round_trips_uid_and_optional_result() {
        let uid = "123456789ABCDEF01122334455667788";
        let command = HostCommand::RemapParameterId {
            old_plugin_uid: uid.to_string(),
            old_param_id: 0xDEAD_BEEF,
        };
        let json = serde_json::to_string(&command).expect("serialize RemapParameterId");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize RemapParameterId") {
            HostCommand::RemapParameterId {
                old_plugin_uid,
                old_param_id,
            } => {
                assert_eq!(old_plugin_uid, uid);
                assert_eq!(old_param_id, 0xDEAD_BEEF);
                assert!(crate::internal::utils::parse_class_uid(&old_plugin_uid).is_some());
            }
            other => panic!("RemapParameterId round-trip changed the variant: {other:?}"),
        }

        for id in [Some(42), None] {
            let response = HostResponse::RemappedParameter { id };
            let json = serde_json::to_string(&response).expect("serialize RemappedParameter");
            match serde_json::from_str::<HostResponse>(&json)
                .expect("deserialize RemappedParameter")
            {
                HostResponse::RemappedParameter { id: decoded } => assert_eq!(decoded, id),
                other => panic!("RemappedParameter round-trip changed the variant: {other:?}"),
            }
        }

        let invalid = HostCommand::RemapParameterId {
            old_plugin_uid: "1234-not-a-uid".to_string(),
            old_param_id: 1,
        };
        let json = serde_json::to_string(&invalid).expect("serialize invalid UID");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize invalid UID") {
            HostCommand::RemapParameterId { old_plugin_uid, .. } => {
                assert!(crate::internal::utils::parse_class_uid(&old_plugin_uid).is_none());
            }
            other => panic!("invalid RemapParameterId changed the variant: {other:?}"),
        }
    }

    #[test]
    fn parameter_edits_round_trip_across_the_wire() {
        // The ordered gesture log must survive the JSON transport host and helper share, both
        // the empty command and the populated reply.
        use crate::plugin::{ParameterEdit, ParameterEditKind};

        let cmd = serde_json::to_string(&HostCommand::TakeParameterEdits)
            .expect("serialize TakeParameterEdits");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&cmd).expect("deserialize TakeParameterEdits"),
            HostCommand::TakeParameterEdits
        ));

        let edits = vec![
            ParameterEdit {
                id: 9,
                kind: ParameterEditKind::BeginGesture,
                value: None,
            },
            ParameterEdit {
                id: 9,
                kind: ParameterEditKind::ValueChange,
                value: Some(0.3),
            },
            ParameterEdit {
                id: 9,
                kind: ParameterEditKind::EndGesture,
                value: None,
            },
        ];
        let resp = HostResponse::ParameterEdits {
            edits: edits.clone(),
        };
        let resp_json = serde_json::to_string(&resp).expect("serialize ParameterEdits");
        match serde_json::from_str::<HostResponse>(&resp_json).expect("deserialize ParameterEdits")
        {
            HostResponse::ParameterEdits { edits: back } => assert_eq!(back, edits),
            other => panic!("ParameterEdits round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn parameter_feedback_round_trips_losslessly_and_is_bounded() {
        let command = serde_json::to_string(&HostCommand::TakeParameterChanges)
            .expect("serialize TakeParameterChanges");
        assert!(matches!(
            serde_json::from_str::<HostCommand>(&command)
                .expect("deserialize TakeParameterChanges"),
            HostCommand::TakeParameterChanges
        ));

        let changes = vec![
            (1, 0.25),
            (2, -0.0),
            (3, f64::NAN),
            (4, f64::INFINITY),
            (5, f64::NEG_INFINITY),
        ];
        let response = HostResponse::ParameterChanges {
            changes: changes.clone(),
        };
        let json = serde_json::to_string(&response).expect("serialize parameter feedback");
        let HostResponse::ParameterChanges { changes: decoded } =
            serde_json::from_str::<HostResponse>(&json).expect("deserialize parameter feedback")
        else {
            panic!("parameter feedback changed response variant");
        };
        assert_eq!(
            decoded
                .iter()
                .map(|&(id, value)| (id, value.to_bits()))
                .collect::<Vec<_>>(),
            changes
                .iter()
                .map(|&(id, value)| (id, value.to_bits()))
                .collect::<Vec<_>>()
        );

        let over_limit = HostResponse::ParameterChanges {
            changes: vec![(1, 0.5); MAX_WIRE_PARAMETER_CHANGES + 1],
        };
        assert!(
            serde_json::to_string(&over_limit).is_err(),
            "the helper must not emit an oversized feedback response"
        );

        let entries = (0..=MAX_WIRE_PARAMETER_CHANGES)
            .map(|_| "[1,0]")
            .collect::<Vec<_>>()
            .join(",");
        let oversized_json = format!("{{\"ParameterChanges\":{{\"changes\":[{entries}]}}}}");
        assert!(
            serde_json::from_str::<HostResponse>(&oversized_json).is_err(),
            "the host must reject oversized feedback before collecting it"
        );
    }

    #[test]
    fn host_notifications_and_restart_requests_round_trip_across_the_wire() {
        use crate::plugin::HostNotification;

        for command in [
            HostCommand::TakeHostNotifications,
            HostCommand::ExecuteContextMenuItem {
                menu_id: 19,
                item_id: 3,
            },
            HostCommand::DismissContextMenu { menu_id: 20 },
            HostCommand::TakeRestartFlags,
            HostCommand::ServiceHostRequests,
        ] {
            let json = serde_json::to_string(&command).expect("serialize host request");
            let decoded = serde_json::from_str::<HostCommand>(&json).expect("deserialize");
            assert_eq!(
                std::mem::discriminant(&decoded),
                std::mem::discriminant(&command)
            );
        }

        let notifications = vec![
            HostNotification::DirtyChanged(true),
            HostNotification::OpenEditorRequested {
                name: Some("editor".to_string()),
            },
            HostNotification::GroupEditStarted,
            HostNotification::GroupEditFinished,
            HostNotification::ContextMenuRequested {
                menu_id: 19,
                parameter_id: Some(44),
                x: 12,
                y: 24,
                items: vec![crate::plugin::ContextMenuItem {
                    item_id: 0,
                    name: "Reset".to_string(),
                    tag: 7,
                    flags: 0,
                }],
            },
        ];
        let response = HostResponse::HostNotifications {
            notifications: notifications.clone(),
        };
        let json = serde_json::to_string(&response).expect("serialize notifications");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize notifications") {
            HostResponse::HostNotifications {
                notifications: decoded,
            } => assert_eq!(decoded, notifications),
            other => panic!("HostNotifications changed variant: {other:?}"),
        }

        let response = HostResponse::RestartFlags { bits: 0x345 };
        let json = serde_json::to_string(&response).expect("serialize restart flags");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize restart flags") {
            HostResponse::RestartFlags { bits } => assert_eq!(bits, 0x345),
            other => panic!("RestartFlags changed variant: {other:?}"),
        }
    }

    #[test]
    fn note_expression_commands_round_trip_across_the_wire() {
        // The MPE commands/responses must survive the JSON transport host and helper share.
        use crate::midi::{NoteExpressionInfo, NoteExpressionType};

        let on = HostCommand::NoteOn {
            channel: 0,
            note: 60,
            velocity: 100,
            sample_offset: 0,
        };
        let on_json = serde_json::to_string(&on).expect("serialize NoteOn");
        match serde_json::from_str::<HostCommand>(&on_json).expect("deserialize NoteOn") {
            HostCommand::NoteOn {
                channel,
                note,
                velocity,
                sample_offset,
            } => {
                assert_eq!((channel, note, velocity, sample_offset), (0, 60, 100, 0));
            }
            other => panic!("NoteOn round-trip changed the variant: {other:?}"),
        }

        let expr = HostCommand::SendNoteExpression {
            note_id: 7,
            kind: NoteExpressionType::Tuning,
            value: 1.0,
            sample_offset: 0,
        };
        let expr_json = serde_json::to_string(&expr).expect("serialize SendNoteExpression");
        match serde_json::from_str::<HostCommand>(&expr_json).expect("deserialize") {
            HostCommand::SendNoteExpression {
                note_id,
                kind,
                value,
                ..
            } => {
                assert_eq!(note_id, 7);
                assert_eq!(kind, NoteExpressionType::Tuning);
                assert_eq!(value, 1.0);
            }
            other => panic!("SendNoteExpression round-trip changed the variant: {other:?}"),
        }

        let started = HostResponse::NoteStarted { note_id: 42 };
        let started_json = serde_json::to_string(&started).expect("serialize NoteStarted");
        match serde_json::from_str::<HostResponse>(&started_json).expect("deserialize") {
            HostResponse::NoteStarted { note_id } => assert_eq!(note_id, 42),
            other => panic!("NoteStarted round-trip changed the variant: {other:?}"),
        }

        let info = NoteExpressionInfo {
            kind: NoteExpressionType::Tuning,
            title: "Tuning".to_string(),
            short_title: "Tun".to_string(),
            units: String::new(),
            default_value: 0.5,
            min: 0.0,
            max: 1.0,
            step_count: 0,
            is_bipolar: true,
            is_one_shot: false,
            is_absolute: false,
        };
        let resp = HostResponse::NoteExpressions {
            expressions: vec![info.clone()],
        };
        let resp_json = serde_json::to_string(&resp).expect("serialize NoteExpressions");
        match serde_json::from_str::<HostResponse>(&resp_json).expect("deserialize") {
            HostResponse::NoteExpressions { expressions } => {
                assert_eq!(expressions, vec![info]);
            }
            other => panic!("NoteExpressions round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn helper_lookup_uses_native_names_and_stays_within_build_locations() {
        let root = std::env::temp_dir().join(format!("vst3-helper-lookup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join("target/debug");
        let deps = profile.join("deps");
        let examples = profile.join("examples");
        let deployed = root.join("app/bin");
        for directory in [&deps, &examples, &deployed] {
            std::fs::create_dir_all(directory).expect("create executable directory");
        }
        let helper_name = format!("vst3-host-helper{}", std::env::consts::EXE_SUFFIX);
        let helper = profile.join(&helper_name);
        std::fs::write(&helper, []).expect("write helper fixture");
        for directory in [&profile, &deps, &examples] {
            assert_eq!(find_helper_binary(directory), Some(helper.clone()));
        }
        // A deployed application must not find an unrelated helper in an ancestor's target/.
        assert_eq!(find_helper_binary(&deployed), None);
        let beside_app = deployed.join(&helper_name);
        std::fs::write(&beside_app, []).expect("write deployed helper fixture");
        assert_eq!(find_helper_binary(&deployed), Some(beside_app));
        std::fs::remove_dir_all(root).expect("remove helper fixtures");
    }

    #[test]
    fn explicit_helper_override_missing_path_reports_clearly() {
        // An explicit helper path that doesn't exist must fail with a clear, path-naming
        // error *before* spawning — not fall through to the heuristic search. This is the
        // observable contract for the builder's `helper_path()` override (roadmap 3.3).
        let bogus = std::path::PathBuf::from("/nonexistent/vst3-host-helper-xyz");
        let err = match PluginHostProcess::new(Some(bogus.clone()), DEFAULT_RESPONSE_TIMEOUT) {
            Ok(_) => panic!("a missing override path must error, not spawn"),
            Err(e) => e,
        };
        assert!(
            err.contains("does not exist"),
            "error should explain the missing path, got: {err}"
        );
        assert!(
            err.contains("vst3-host-helper-xyz"),
            "error should name the offending path, got: {err}"
        );
    }

    #[test]
    fn non_finite_samples_survive_the_audio_wire_format() {
        // JSON has no spelling for NaN/±∞ — serde_json writes `null`, which will not
        // deserialize back into an f32 — so one such sample used to fail every Process
        // exchange. The bit-pattern encoding carries them (and -0.0) exactly.
        let channel = vec![
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -0.0,
            0.5,
            f32::MIN_POSITIVE,
        ];
        let resp = HostResponse::AudioOutput {
            outputs: vec![channel.clone(), vec![]],
            output_events: Vec::new(),
        };
        let json = serde_json::to_string(&resp).expect("serialize");
        assert!(
            !json.contains("null"),
            "non-finite samples must not become null"
        );
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize") {
            HostResponse::AudioOutput { outputs, .. } => {
                assert_eq!(outputs.len(), 2);
                assert!(outputs[1].is_empty());
                let bits: Vec<u32> = outputs[0].iter().map(|s| s.to_bits()).collect();
                let want: Vec<u32> = channel.iter().map(|s| s.to_bits()).collect();
                assert_eq!(bits, want, "samples must round-trip bit-exactly");
            }
            other => panic!("round-trip changed the variant: {other:?}"),
        }

        // The same for the host -> helper direction.
        let cmd = HostCommand::Process {
            inputs: vec![vec![f32::NAN, 1.0]],
            frames: 2,
        };
        let json = serde_json::to_string(&cmd).expect("serialize Process");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize Process") {
            HostCommand::Process { inputs, frames } => {
                assert_eq!(frames, 2);
                assert!(inputs[0][0].is_nan());
                assert_eq!(inputs[0][1], 1.0);
            }
            other => panic!("Process round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn bus_audio_wire_preserves_bus_boundaries_activation_and_sample_bits() {
        let command = HostCommand::ProcessBuses {
            inputs: vec![
                crate::audio::AudioBusBuffer {
                    active: true,
                    channels: vec![vec![f32::NAN, 1.0], vec![2.0, 3.0]],
                },
                crate::audio::AudioBusBuffer {
                    active: false,
                    channels: vec![vec![99.0, 99.0]],
                },
            ],
            outputs: vec![
                crate::audio::AudioBusConfig {
                    channel_count: 2,
                    active: true,
                },
                crate::audio::AudioBusConfig {
                    channel_count: 1,
                    active: false,
                },
            ],
            frames: 2,
        };
        let json = serde_json::to_string(&command).expect("serialize ProcessBuses");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize ProcessBuses") {
            HostCommand::ProcessBuses {
                inputs,
                outputs,
                frames,
            } => {
                assert_eq!(frames, 2);
                assert_eq!(inputs.len(), 2);
                assert!(inputs[0].active);
                assert!(!inputs[1].active);
                assert!(inputs[0].channels[0][0].is_nan());
                assert_eq!(inputs[1].channels[0], [99.0, 99.0]);
                assert_eq!(outputs[1].channel_count, 1);
                assert!(!outputs[1].active);
            }
            other => panic!("ProcessBuses round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn non_finite_parameter_values_survive_the_wire() {
        // A plugin can hand back a non-finite normalized value; it must not poison the
        // exchange. Finite values stay plain JSON numbers.
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let json = serde_json::to_string(&HostResponse::ParameterValue { value })
                .expect("serialize ParameterValue");
            match serde_json::from_str::<HostResponse>(&json).expect("deserialize") {
                HostResponse::ParameterValue { value: back } => {
                    if value.is_nan() {
                        assert!(back.is_nan(), "NaN must survive the wire");
                    } else {
                        assert_eq!(back, value);
                    }
                }
                other => panic!("round-trip changed the variant: {other:?}"),
            }
        }

        let json = serde_json::to_string(&HostResponse::ParameterValue { value: 0.25 })
            .expect("serialize");
        assert!(
            json.contains("0.25") && !json.contains("\"0.25\""),
            "finite values stay JSON numbers, got {json}"
        );
        let cmd = HostCommand::SetParameter {
            id: 3,
            value: f64::NAN,
        };
        let json = serde_json::to_string(&cmd).expect("serialize SetParameter");
        match serde_json::from_str::<HostCommand>(&json).expect("deserialize") {
            HostCommand::SetParameter { id, value } => {
                assert_eq!(id, 3);
                assert!(value.is_nan());
            }
            other => panic!("SetParameter round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn audio_codec_round_trips_and_shrinks_the_payload() {
        // Every length (including the two partial base64 groups) must round-trip.
        for len in 0..9usize {
            let samples: Vec<f32> = (0..len).map(|i| i as f32 * -0.3125).collect();
            let encoded = audio_codec::encode_channel(&samples);
            let decoded = audio_codec::decode_channel(&encoded).expect("decode");
            assert_eq!(decoded, samples, "round-trip failed at len {len}");
        }
        assert!(audio_codec::decode_channel("!!!!").is_none());
        assert!(audio_codec::decode_channel("AAA").is_none(), "bad length");
        assert!(
            audio_codec::decode_channel("AAAA").is_none(),
            "3 bytes is not a whole f32"
        );

        // The bit-pattern form is also a good deal smaller than a JSON number array.
        let block: Vec<Vec<f32>> = (0..2)
            .map(|c| {
                (0..512)
                    .map(|i| ((i * 7 + c) as f32 / 512.0).sin())
                    .collect()
            })
            .collect();
        let plain = serde_json::to_string(&block).expect("plain json").len();
        let encoded = serde_json::to_string(&HostResponse::AudioOutput {
            outputs: block,
            output_events: Vec::new(),
        })
        .expect("encoded json")
        .len();
        assert!(
            encoded < plain,
            "base64 payload ({encoded}) should be smaller than the number array ({plain})"
        );
    }

    #[test]
    fn wire_provided_counts_are_clamped_on_receipt() {
        // The host sizes buffers from these numbers, so a bogus helper reply must not be
        // taken at face value.
        let json = r#"{"PluginInfo":{"vendor":"v","name":"n","version":"1","category":"",
            "uid":"u","has_gui":false,"audio_inputs":-4,"audio_outputs":999999,
            "output_channels":2000000,"has_midi_input":true,"has_midi_output":false}}"#;
        match serde_json::from_str::<HostResponse>(json).expect("deserialize PluginInfo") {
            HostResponse::PluginInfo {
                audio_inputs,
                audio_outputs,
                output_channels,
                ..
            } => {
                assert_eq!(audio_inputs, 0);
                assert_eq!(audio_outputs, MAX_WIRE_BUSES);
                assert_eq!(output_channels, MAX_WIRE_CHANNELS as i32);
            }
            other => panic!("PluginInfo round-trip changed the variant: {other:?}"),
        }

        // Channel counts on the audio payload are clamped the same way.
        let channels: Vec<String> = (0..MAX_WIRE_CHANNELS + 5)
            .map(|_| audio_codec::encode_channel(&[0.0]))
            .collect();
        let json = serde_json::to_string(&serde_json::json!({
            "AudioOutput": { "outputs": channels, "output_events": [] }
        }))
        .expect("serialize");
        match serde_json::from_str::<HostResponse>(&json).expect("deserialize") {
            HostResponse::AudioOutput { outputs, .. } => {
                assert_eq!(outputs.len(), MAX_WIRE_CHANNELS)
            }
            other => panic!("AudioOutput round-trip changed the variant: {other:?}"),
        }
    }

    #[test]
    fn oversized_lines_are_discarded_rather_than_buffered() {
        // A helper that never terminates a line must not be able to grow host memory.
        let mut input: Vec<u8> = Vec::new();
        input.extend_from_slice(b"short\n");
        input.extend_from_slice(&[b'x'; 64]);
        input.push(b'\n');
        input.extend_from_slice(b"ok\n");
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));

        assert!(matches!(read_bounded_line(&mut reader, 8), ReadLine::Line(l) if l == b"short\n"));
        assert!(matches!(
            read_bounded_line(&mut reader, 8),
            ReadLine::Oversized
        ));
        assert!(matches!(read_bounded_line(&mut reader, 8), ReadLine::Line(l) if l == b"ok\n"));
        assert!(matches!(read_bounded_line(&mut reader, 8), ReadLine::Eof));
    }

    #[test]
    fn slow_commands_are_classified_apart_from_the_per_block_ones() {
        assert!(is_slow_command(&HostCommand::SaveState));
        assert!(is_slow_command(&HostCommand::LoadState {
            data: vec![],
            context: crate::plugin::StateContext::Project,
        }));
        assert!(is_slow_command(&HostCommand::LoadPlugin {
            path: "x".into(),
            sample_rate: 44100.0,
            block_size: 512,
            tempo: 120.0,
            time_sig_numerator: 4,
            time_sig_denominator: 4,
            class_id: None,
        }));
        assert!(!is_slow_command(&HostCommand::Process {
            inputs: vec![],
            frames: 64
        }));
        assert!(!is_slow_command(&HostCommand::GetAllParameters));
    }

    /// A helper that never responds (a hung plugin) must not hang the host: `send_command`
    /// returns an error within the timeout and kills the child.
    #[cfg(unix)]
    #[test]
    fn hung_helper_times_out_and_is_killed_not_blocking() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        // Fake helper: read nothing, write nothing, just sleep — i.e. hang forever.
        let dir = std::env::temp_dir().join(format!("vst3_hang_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("hung-helper");
        let mut f = std::fs::File::create(&fake).unwrap();
        // `exec` so the shell is replaced by sleep (no orphaned child holding the stdout
        // pipe); killing the helper then closes the pipe and ends the reader thread promptly.
        writeln!(f, "#!/bin/sh\nexec sleep 30").unwrap();
        drop(f);
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut proc =
            PluginHostProcess::spawn(fake.clone(), Duration::from_millis(200)).expect("spawn");
        let started = Instant::now();
        let res = proc.send_command(HostCommand::Shutdown);
        let elapsed = started.elapsed();

        assert!(
            res.is_err(),
            "a hung helper must yield an error, got {res:?}"
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "send_command must return promptly on timeout, took {elapsed:?}"
        );
        // The child was killed; a follow-up command also errors rather than hanging.
        assert!(proc.send_command(HostCommand::Shutdown).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A helper that dies on its Nth load and answers otherwise, so a test can pin exactly how
    /// many load attempts the host makes. `crashing_loads` is how many of the first loads —
    /// counted across every process spawned from this script — end in a killed helper.
    #[cfg(unix)]
    struct FlakyLoadHelper {
        dir: std::path::PathBuf,
        script: std::path::PathBuf,
        attempts: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl FlakyLoadHelper {
        fn new(name: &str, crashing_loads: u32) -> Self {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;

            let dir = std::env::temp_dir().join(format!(
                "vst3_flaky_{name}_{}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp dir");
            let script = dir.join("flaky-helper");
            let attempts = dir.join("attempts");

            let mut f = std::fs::File::create(&script).expect("create script");
            // Every LoadPlugin bumps a shared counter; the first `crashing_loads` of them make
            // the helper exit without answering, which is what a plugin crashing inside its own
            // initialization looks like from the host's side.
            write!(
                f,
                "#!/bin/sh\n\
                 while IFS= read -r line; do\n\
                 \x20 case \"$line\" in\n\
                 \x20   *LoadPlugin*)\n\
                 \x20     n=$(cat '{attempts}' 2>/dev/null || echo 0)\n\
                 \x20     n=$((n+1))\n\
                 \x20     printf '%s' \"$n\" > '{attempts}'\n\
                 \x20     if [ \"$n\" -le {crashing_loads} ]; then exit 3; fi\n\
                 \x20     printf '%s\\n' '{{\"PluginInfo\":{{\"vendor\":\"v\",\"name\":\"n\",\"version\":\"1\",\"category\":\"\",\"uid\":\"u\",\"has_gui\":false,\"audio_inputs\":0,\"audio_outputs\":1,\"output_channels\":2,\"has_midi_input\":true,\"has_midi_output\":false}}}}' ;;\n\
                 \x20   *) exit 3 ;;\n\
                 \x20 esac\n\
                 done\n",
                attempts = attempts.display(),
                crashing_loads = crashing_loads,
            )
            .expect("write script");
            drop(f);
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
            Self {
                dir,
                script,
                attempts,
            }
        }

        fn load_attempts(&self) -> u32 {
            std::fs::read_to_string(&self.attempts)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0)
        }

        fn load_command() -> HostCommand {
            HostCommand::LoadPlugin {
                path: "/tmp/flaky.vst3".to_string(),
                sample_rate: 44100.0,
                block_size: 512,
                tempo: 120.0,
                time_sig_numerator: 4,
                time_sig_denominator: 4,
                class_id: None,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for FlakyLoadHelper {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Real plugins lose races inside their own cold-start initialization and take the helper
    /// down with them. A fresh helper is an independent roll and the dead one held nothing, so
    /// the load is replayed rather than reported.
    #[cfg(unix)]
    #[test]
    fn a_load_that_kills_the_helper_is_replayed_against_a_fresh_one() {
        let fake = FlakyLoadHelper::new("recovers", 1);
        let mut proc = PluginHostProcess::spawn(fake.script.clone(), Duration::from_secs(5))
            .expect("spawn flaky helper");
        let first_pid = proc.helper_pid().expect("helper pid");

        let response = proc
            .send_command(FlakyLoadHelper::load_command())
            .expect("a crashed load must be retried, not reported");
        assert!(matches!(response, HostResponse::PluginInfo { .. }));
        assert_eq!(fake.load_attempts(), 2, "the load should be tried twice");
        assert_ne!(
            proc.helper_pid().expect("helper pid after retry"),
            first_pid,
            "the retry must run against a freshly spawned helper"
        );
        assert!(proc.is_alive(), "the handle must be usable after the retry");
    }

    /// The retry is bounded: a plugin that genuinely cannot load reports that after one extra
    /// attempt rather than respawning forever.
    #[cfg(unix)]
    #[test]
    fn a_load_that_always_crashes_gives_up_after_one_retry() {
        let fake = FlakyLoadHelper::new("always", 99);
        let mut proc = PluginHostProcess::spawn(fake.script.clone(), Duration::from_secs(5))
            .expect("spawn flaky helper");

        let error = proc
            .send_command(FlakyLoadHelper::load_command())
            .expect_err("a load that always crashes must still fail");
        assert!(
            error.to_lowercase().contains("crash") || error.to_lowercase().contains("exited"),
            "the reported failure must still read as a crash, got {error}"
        );
        assert_eq!(
            fake.load_attempts(),
            1 + LOAD_CRASH_RETRIES,
            "exactly one retry, no more"
        );
    }

    /// Only the load is replayed. Every other command runs against a helper holding live
    /// plugin state, which a fresh process would not have — silently redoing those would hand
    /// the caller a default-initialized plugin dressed up as a success.
    #[cfg(unix)]
    #[test]
    fn a_crash_on_any_other_command_is_reported_not_retried() {
        let fake = FlakyLoadHelper::new("other", 0);
        let mut proc = PluginHostProcess::spawn(fake.script.clone(), Duration::from_secs(5))
            .expect("spawn flaky helper");
        let pid = proc.helper_pid().expect("helper pid");

        assert!(
            proc.send_command(HostCommand::GetAllParameters).is_err(),
            "a helper that dies mid-command must surface as an error"
        );
        assert_eq!(
            proc.helper_pid(),
            Some(pid),
            "no other command may respawn the helper"
        );
        assert!(!proc.is_alive());
    }
}

/// Crash protection utilities for in-process plugins
pub mod crash_protection {
    use std::panic::catch_unwind;
    use std::panic::UnwindSafe;
    use std::time::Duration;

    /// Status of a plugin after a protected call
    #[derive(Debug, Clone, PartialEq)]
    pub enum PluginStatus {
        /// Plugin executed successfully
        Ok,
        /// Plugin crashed with panic
        Crashed(String),
        /// Plugin took too long to execute
        Timeout(Duration),
    }

    /// Execute a function with panic protection
    pub fn protected_call<F, R>(f: F) -> Result<R, String>
    where
        F: FnOnce() -> R + UnwindSafe,
    {
        catch_unwind(f).map_err(|e| {
            if let Some(s) = e.downcast_ref::<&str>() {
                format!("Plugin panicked: {}", s)
            } else if let Some(s) = e.downcast_ref::<String>() {
                format!("Plugin panicked: {}", s)
            } else {
                "Plugin panicked with unknown error".to_string()
            }
        })
    }
}
