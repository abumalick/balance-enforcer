//! Shared mock audio controller used by `integration.rs`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crossbeam_channel::{bounded, Receiver, Sender};

use balance_enforcer::audio::{AudioController, AudioError, AudioEvent, AudioResult};

pub struct MockAudio {
    channels: Mutex<Vec<f32>>,
    writes: Mutex<Vec<(u32, f32)>>,
    tx: Sender<AudioEvent>,
    rx: Mutex<Option<Receiver<AudioEvent>>>,
    fail_next_get: AtomicBool,
}

#[allow(dead_code)]
impl MockAudio {
    pub fn stereo(left: f32, right: f32) -> Self {
        let (tx, rx) = bounded(256);
        Self {
            channels: Mutex::new(vec![left, right]),
            writes: Mutex::new(Vec::new()),
            tx,
            rx: Mutex::new(Some(rx)),
            fail_next_get: AtomicBool::new(false),
        }
    }

    pub fn with_channel_count(count: u32) -> Self {
        let (tx, rx) = bounded(256);
        Self {
            channels: Mutex::new(vec![0.5; count as usize]),
            writes: Mutex::new(Vec::new()),
            tx,
            rx: Mutex::new(Some(rx)),
            fail_next_get: AtomicBool::new(false),
        }
    }

    pub fn writes(&self) -> Vec<(u32, f32)> {
        self.writes.lock().unwrap().clone()
    }

    pub fn channels(&self) -> Vec<f32> {
        self.channels.lock().unwrap().clone()
    }

    pub fn set_channels(&self, channels: Vec<f32>) {
        *self.channels.lock().unwrap() = channels;
    }

    pub fn fail_next_get(&self) {
        self.fail_next_get.store(true, Ordering::SeqCst);
    }

    pub fn push_event(&self, event: AudioEvent) {
        self.tx.send(event).expect("test channel full");
    }

    pub fn sender(&self) -> Sender<AudioEvent> {
        self.tx.clone()
    }
}

impl AudioController for MockAudio {
    fn channel_count(&self) -> AudioResult<u32> {
        Ok(self.channels.lock().unwrap().len() as u32)
    }

    fn get_channel(&self, channel: u32) -> AudioResult<f32> {
        if self.fail_next_get.swap(false, Ordering::SeqCst) {
            return Err(AudioError::GetLevel {
                channel,
                message: "injected fault".into(),
            });
        }
        let guard = self.channels.lock().unwrap();
        guard
            .get(channel as usize)
            .copied()
            .ok_or(AudioError::UnsupportedChannelCount(guard.len() as u32))
    }

    fn set_channel(&self, channel: u32, level: f32) -> AudioResult<()> {
        self.writes.lock().unwrap().push((channel, level));
        let mut guard = self.channels.lock().unwrap();
        if let Some(slot) = guard.get_mut(channel as usize) {
            *slot = level;
        }
        Ok(())
    }

    fn subscribe(&self) -> AudioResult<Receiver<AudioEvent>> {
        self.rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| AudioError::CallbackRegistration("subscribe already called".into()))
    }
}
