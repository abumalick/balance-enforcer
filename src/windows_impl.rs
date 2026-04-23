//! Windows-specific audio adapter backed by the Core Audio / MMDevice APIs.
//!
//! A stub implementation is provided on non-Windows targets so that `cargo check`
//! and the portable logic tests continue to build on macOS and Linux.

#[cfg(target_os = "windows")]
pub use self::real::{enumerate_render_endpoints, EndpointInfo, RealAudio};

#[cfg(not(target_os = "windows"))]
pub use self::stub::{enumerate_render_endpoints, EndpointInfo, RealAudio};

// ---------------- Windows implementation ----------------

#[cfg(target_os = "windows")]
mod real {
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;

    use crossbeam_channel::{bounded, Receiver, Sender};
    use tracing::{debug, info, warn};
    use windows::core::{implement, Result as WinResult, GUID, PCWSTR};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::Endpoints::{
        IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
    };
    use windows::Win32::Media::Audio::{
        eMultimedia, eRender, IMMDevice, IMMDeviceEnumerator, IMMNotificationClient,
        IMMNotificationClient_Impl, MMDeviceEnumerator, AUDIO_VOLUME_NOTIFICATION_DATA,
        DEVICE_STATE, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::PROPERTYKEY;
    use windows_core::PROPVARIANT;

    use crate::audio::{AudioController, AudioError, AudioEvent, AudioResult};

    static COM_INIT: OnceLock<()> = OnceLock::new();

    fn ensure_com_initialized() -> AudioResult<()> {
        let mut first = None;
        COM_INIT.get_or_init(|| {
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_err() {
                first = Some(format!("CoInitializeEx returned {hr:?}"));
            }
        });
        match first {
            Some(msg) => Err(AudioError::ComInit(msg)),
            None => Ok(()),
        }
    }

    fn to_com_err(e: windows::core::Error, ctx: &str) -> AudioError {
        AudioError::Activate(format!("{ctx}: {e}"))
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn pwstr_to_string(p: PCWSTR) -> String {
        if p.0.is_null() {
            return String::new();
        }
        unsafe {
            let mut len = 0usize;
            while *p.0.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
        }
    }

    #[derive(Debug, Clone)]
    pub struct EndpointInfo {
        pub id: String,
        pub friendly_name: String,
        pub is_default: bool,
    }

    pub fn enumerate_render_endpoints() -> AudioResult<Vec<EndpointInfo>> {
        ensure_com_initialized()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(|e| to_com_err(e, "CoCreateInstance(MMDeviceEnumerator)"))?;

            let default_id = enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .ok()
                .and_then(|d| d.GetId().ok())
                .map(|p| {
                    let s = pwstr_to_string(PCWSTR(p.as_ptr()));
                    windows::Win32::System::Com::CoTaskMemFree(Some(p.as_ptr().cast()));
                    s
                })
                .unwrap_or_default();

            let collection = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(|e| to_com_err(e, "EnumAudioEndpoints"))?;

            let count = collection
                .GetCount()
                .map_err(|e| to_com_err(e, "GetCount"))?;

            let mut endpoints = Vec::with_capacity(count as usize);
            for i in 0..count {
                let device = collection.Item(i).map_err(|e| to_com_err(e, "Item"))?;
                let id_raw = device.GetId().map_err(|e| to_com_err(e, "GetId"))?;
                let id = pwstr_to_string(PCWSTR(id_raw.as_ptr()));
                windows::Win32::System::Com::CoTaskMemFree(Some(id_raw.as_ptr().cast()));

                let friendly = friendly_name(&device).unwrap_or_else(|_| "<unnamed>".into());

                endpoints.push(EndpointInfo {
                    is_default: id == default_id,
                    id,
                    friendly_name: friendly,
                });
            }
            Ok(endpoints)
        }
    }

    unsafe fn friendly_name(device: &IMMDevice) -> AudioResult<String> {
        let store = device
            .OpenPropertyStore(STGM_READ)
            .map_err(|e| to_com_err(e, "OpenPropertyStore"))?;
        let prop: PROPVARIANT = store
            .GetValue(&PKEY_Device_FriendlyName as *const PROPERTYKEY)
            .map_err(|e| to_com_err(e, "GetValue(FriendlyName)"))?;
        let s = prop.to_string();
        Ok(s)
    }

    /// Holds the live COM objects for the pinned device plus the event plumbing.
    pub struct RealAudio {
        inner: Arc<Inner>,
    }

    struct Inner {
        device_id: Vec<u16>,
        state: Mutex<DeviceState>,
        tx: Sender<AudioEvent>,
        rx_take: Mutex<Option<Receiver<AudioEvent>>>,
    }

    struct DeviceState {
        endpoint: Option<IAudioEndpointVolume>,
        volume_cb: Option<IAudioEndpointVolumeCallback>,
        notif_client: Option<IMMNotificationClient>,
        enumerator: Option<IMMDeviceEnumerator>,
    }

    // Safety: the held COM interfaces are agile in Core Audio (MTA-compatible)
    // and we only touch them under the `Mutex`. The bounded channel is Send+Sync.
    unsafe impl Send for DeviceState {}
    unsafe impl Sync for DeviceState {}

    impl RealAudio {
        pub fn for_device(device_id: &str) -> AudioResult<Self> {
            ensure_com_initialized()?;

            let (tx, rx) = bounded::<AudioEvent>(256);
            let inner = Arc::new(Inner {
                device_id: wide(device_id),
                state: Mutex::new(DeviceState {
                    endpoint: None,
                    volume_cb: None,
                    notif_client: None,
                    enumerator: None,
                }),
                tx: tx.clone(),
                rx_take: Mutex::new(Some(rx)),
            });

            // Initial bind, with retry loop (device might not be ready yet at logon).
            Self::bind_with_retry(&inner)?;

            // Spin up the watchdog that periodically emits Tick events.
            let tx_watch = tx.clone();
            thread::Builder::new()
                .name("balance-enforcer-watchdog".into())
                .spawn(move || loop {
                    thread::sleep(Duration::from_millis(500));
                    if tx_watch.send(AudioEvent::Tick).is_err() {
                        break;
                    }
                })
                .map_err(AudioError::from)?;

            Ok(Self { inner })
        }

        fn bind_with_retry(inner: &Arc<Inner>) -> AudioResult<()> {
            let mut backoff_ms: u64 = 100;
            loop {
                match Self::try_bind(inner) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        warn!(error = %e, backoff_ms, "initial bind failed; retrying");
                        thread::sleep(Duration::from_millis(backoff_ms));
                        backoff_ms = (backoff_ms * 2).min(5_000);
                    }
                }
            }
        }

        fn try_bind(inner: &Arc<Inner>) -> AudioResult<()> {
            unsafe {
                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                        .map_err(|e| to_com_err(e, "CoCreateInstance(MMDeviceEnumerator)"))?;

                let device: IMMDevice = enumerator
                    .GetDevice(PCWSTR(inner.device_id.as_ptr()))
                    .map_err(|e| to_com_err(e, "GetDevice(target_id)"))?;

                let endpoint: IAudioEndpointVolume = device
                    .Activate(CLSCTX_ALL, None)
                    .map_err(|e| to_com_err(e, "Activate(IAudioEndpointVolume)"))?;

                let volume_cb: IAudioEndpointVolumeCallback = VolumeCallback {
                    tx: inner.tx.clone(),
                }
                .into();
                endpoint
                    .RegisterControlChangeNotify(&volume_cb)
                    .map_err(|e| {
                        AudioError::CallbackRegistration(format!(
                            "RegisterControlChangeNotify: {e}"
                        ))
                    })?;

                let notif_client: IMMNotificationClient = NotificationClient {
                    tx: inner.tx.clone(),
                    target_id: inner.device_id.clone(),
                }
                .into();
                enumerator
                    .RegisterEndpointNotificationCallback(&notif_client)
                    .map_err(|e| {
                        AudioError::CallbackRegistration(format!(
                            "RegisterEndpointNotificationCallback: {e}"
                        ))
                    })?;

                let mut state = inner.state.lock().expect("state poisoned");
                // Release any previously-registered callbacks before swapping in the new ones.
                if let (Some(old_ep), Some(old_cb)) =
                    (state.endpoint.take(), state.volume_cb.take())
                {
                    let _ = old_ep.UnregisterControlChangeNotify(&old_cb);
                }
                if let (Some(old_enum), Some(old_notif)) =
                    (state.enumerator.take(), state.notif_client.take())
                {
                    let _ = old_enum.UnregisterEndpointNotificationCallback(&old_notif);
                }
                state.endpoint = Some(endpoint);
                state.volume_cb = Some(volume_cb);
                state.enumerator = Some(enumerator);
                state.notif_client = Some(notif_client);

                info!("bound to target audio endpoint");
                Ok(())
            }
        }

        fn with_endpoint<T>(
            &self,
            f: impl FnOnce(&IAudioEndpointVolume) -> WinResult<T>,
        ) -> AudioResult<T> {
            let state = self.inner.state.lock().expect("state poisoned");
            let endpoint = state
                .endpoint
                .as_ref()
                .ok_or(AudioError::DeviceDisappeared)?;
            f(endpoint).map_err(|e| AudioError::Activate(format!("endpoint op: {e}")))
        }
    }

    impl AudioController for RealAudio {
        fn channel_count(&self) -> AudioResult<u32> {
            self.with_endpoint(|ep| unsafe { ep.GetChannelCount() })
        }

        fn get_channel(&self, channel: u32) -> AudioResult<f32> {
            let state = self.inner.state.lock().expect("state poisoned");
            let endpoint = state
                .endpoint
                .as_ref()
                .ok_or(AudioError::DeviceDisappeared)?;
            unsafe { endpoint.GetChannelVolumeLevelScalar(channel) }.map_err(|e| {
                AudioError::GetLevel {
                    channel,
                    message: e.to_string(),
                }
            })
        }

        fn set_channel(&self, channel: u32, level: f32) -> AudioResult<()> {
            let state = self.inner.state.lock().expect("state poisoned");
            let endpoint = state
                .endpoint
                .as_ref()
                .ok_or(AudioError::DeviceDisappeared)?;
            unsafe {
                endpoint
                    .SetChannelVolumeLevelScalar(channel, level, &GUID::zeroed())
                    .map_err(|e| AudioError::SetLevel {
                        channel,
                        message: e.to_string(),
                    })
            }
        }

        fn subscribe(&self) -> AudioResult<Receiver<AudioEvent>> {
            self.inner
                .rx_take
                .lock()
                .expect("rx_take poisoned")
                .take()
                .ok_or_else(|| {
                    AudioError::CallbackRegistration("subscribe() already called".into())
                })
        }
    }

    impl Drop for RealAudio {
        fn drop(&mut self) {
            let mut state = self.inner.state.lock().unwrap_or_else(|p| p.into_inner());
            if let (Some(ep), Some(cb)) = (state.endpoint.take(), state.volume_cb.take()) {
                unsafe {
                    let _ = ep.UnregisterControlChangeNotify(&cb);
                }
            }
            if let (Some(enm), Some(nc)) = (state.enumerator.take(), state.notif_client.take()) {
                unsafe {
                    let _ = enm.UnregisterEndpointNotificationCallback(&nc);
                }
            }
        }
    }

    // ---- COM callbacks ----

    #[implement(IAudioEndpointVolumeCallback)]
    struct VolumeCallback {
        tx: Sender<AudioEvent>,
    }

    impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
        fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> WinResult<()> {
            // Never call back into the COM stack from here — just forward an
            // event and let the reconciler thread own all writes.
            let _ = self.tx.try_send(AudioEvent::VolumeChanged);
            Ok(())
        }
    }

    #[implement(IMMNotificationClient)]
    struct NotificationClient {
        tx: Sender<AudioEvent>,
        /// Wide-string of our target device ID. We only care about events that
        /// concern this specific device.
        target_id: Vec<u16>,
    }

    impl NotificationClient_Impl {
        fn id_matches(&self, id: &PCWSTR) -> bool {
            if id.0.is_null() {
                return false;
            }
            let target = &self.target_id;
            unsafe {
                for (i, &t) in target.iter().enumerate() {
                    let c = *id.0.add(i);
                    if c != t {
                        return false;
                    }
                    if t == 0 {
                        return true;
                    }
                }
            }
            true
        }
    }

    impl IMMNotificationClient_Impl for NotificationClient_Impl {
        fn OnDeviceStateChanged(
            &self,
            device_id: &PCWSTR,
            new_state: DEVICE_STATE,
        ) -> WinResult<()> {
            if self.id_matches(device_id) && new_state == DEVICE_STATE_ACTIVE {
                debug!("target device became active; emitting DeviceReappeared");
                let _ = self.tx.try_send(AudioEvent::DeviceReappeared);
            }
            Ok(())
        }

        fn OnDeviceAdded(&self, device_id: &PCWSTR) -> WinResult<()> {
            if self.id_matches(device_id) {
                let _ = self.tx.try_send(AudioEvent::DeviceReappeared);
            }
            Ok(())
        }

        fn OnDeviceRemoved(&self, _device_id: &PCWSTR) -> WinResult<()> {
            Ok(())
        }

        fn OnDefaultDeviceChanged(
            &self,
            _flow: windows::Win32::Media::Audio::EDataFlow,
            _role: windows::Win32::Media::Audio::ERole,
            _default_device_id: &PCWSTR,
        ) -> WinResult<()> {
            Ok(())
        }

        fn OnPropertyValueChanged(&self, _device_id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
            Ok(())
        }
    }
}

// ---------------- Non-Windows stub ----------------

#[cfg(not(target_os = "windows"))]
mod stub {
    use crossbeam_channel::Receiver;

    use crate::audio::{AudioController, AudioError, AudioEvent, AudioResult};

    #[derive(Debug, Clone)]
    pub struct EndpointInfo {
        pub id: String,
        pub friendly_name: String,
        pub is_default: bool,
    }

    pub fn enumerate_render_endpoints() -> AudioResult<Vec<EndpointInfo>> {
        Err(AudioError::Unsupported)
    }

    pub struct RealAudio;

    impl RealAudio {
        pub fn for_device(_device_id: &str) -> AudioResult<Self> {
            Err(AudioError::Unsupported)
        }
    }

    impl AudioController for RealAudio {
        fn channel_count(&self) -> AudioResult<u32> {
            Err(AudioError::Unsupported)
        }
        fn get_channel(&self, _channel: u32) -> AudioResult<f32> {
            Err(AudioError::Unsupported)
        }
        fn set_channel(&self, _channel: u32, _level: f32) -> AudioResult<()> {
            Err(AudioError::Unsupported)
        }
        fn subscribe(&self) -> AudioResult<Receiver<AudioEvent>> {
            Err(AudioError::Unsupported)
        }
    }
}
