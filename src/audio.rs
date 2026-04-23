//! Portable audio abstraction. No OS-specific types leak through this module.

use crossbeam_channel::Receiver;
use thiserror::Error;

pub type AudioResult<T> = Result<T, AudioError>;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("COM initialization failed: {0}")]
    ComInit(String),

    #[error("failed to activate audio endpoint: {0}")]
    Activate(String),

    #[error("failed to read channel {channel} level: {message}")]
    GetLevel { channel: u32, message: String },

    #[error("failed to write channel {channel} level: {message}")]
    SetLevel { channel: u32, message: String },

    #[error("unsupported channel count: {0} (only stereo is handled)")]
    UnsupportedChannelCount(u32),

    #[error("target device is not present")]
    DeviceDisappeared,

    #[error("no target device configured; run `--install` first")]
    TargetNotConfigured,

    #[error("audio backend is not supported on this platform")]
    Unsupported,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("registration of audio callback failed: {0}")]
    CallbackRegistration(String),
}

/// Events delivered to the enforcer's reconciliation loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioEvent {
    /// The endpoint fired a volume-change notification.
    VolumeChanged,
    /// The target device was removed and has just reappeared.
    DeviceReappeared,
    /// Periodic watchdog tick (fallback in case callbacks stop firing).
    Tick,
}

/// Policy that defines the per-channel levels to enforce.
///
/// `tolerance` is the minimum absolute deviation that triggers a write; any drift
/// smaller than this is ignored to prevent write-loop storms between the daemon
/// and the Windows audio engine's own re-equalization.
#[derive(Debug, Clone, Copy)]
pub struct BalancePolicy {
    pub left: f32,
    pub right: f32,
    pub tolerance: f32,
}

impl BalancePolicy {
    pub const fn mute_left() -> Self {
        Self {
            left: 0.0,
            right: 1.0,
            tolerance: 0.02,
        }
    }
}

/// Trait exposing the bits of the OS audio API that the enforcer depends on.
///
/// Implementations must be thread-safe because the enforcer spins up a worker
/// thread that reads and writes through this trait.
pub trait AudioController: Send + Sync {
    /// Number of channels on the underlying endpoint (typically 2 for stereo).
    fn channel_count(&self) -> AudioResult<u32>;

    /// Read the current scalar level (0.0 – 1.0) of a specific channel.
    fn get_channel(&self, channel: u32) -> AudioResult<f32>;

    /// Write the scalar level (0.0 – 1.0) of a specific channel.
    fn set_channel(&self, channel: u32, level: f32) -> AudioResult<()>;

    /// Returns a receiver that yields `AudioEvent`s from the OS.
    ///
    /// Implementations typically wire this up to platform callbacks (e.g. the
    /// `IAudioEndpointVolumeCallback` interface on Windows) plus a watchdog
    /// timer. Events MUST NOT be dispatched from the same thread that the
    /// enforcer uses for writes.
    fn subscribe(&self) -> AudioResult<Receiver<AudioEvent>>;
}
