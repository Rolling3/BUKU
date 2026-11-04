//! Master-volume change notifications for the default playback device.
//!
//! Core Audio calls these callbacks on its own threads, so they only post a
//! message to the notch window and never touch UI state.

use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, eMultimedia, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
use windows::core::{PCWSTR, Result, implement};

use crate::msg::{self, WindowHandle};

/// `lparam` bits of `msg::VOLUME` and `msg::APP_VOLUME`. Bits 8 and up of
/// `msg::VOLUME` carry the device generation.
pub const VOLUME_MUTED: isize = 1;
/// Set for changes BUKU made itself.
pub const VOLUME_OWN: isize = 2;
const GENERATION_SHIFT: u32 = 8;

/// The device (or session set) generation a volume message was posted for.
pub fn volume_generation(flags: isize) -> u32 {
    (flags >> GENERATION_SHIFT) as u32 & 0xff
}

/// `lparam` bits that carry `generation`; see `volume_generation`.
pub fn generation_flags(generation: u32) -> isize {
    ((generation & 0xff) as isize) << GENERATION_SHIFT
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback {
    window: WindowHandle,
    /// Which attached device this callback belongs to, so a notification still
    /// queued from the previous default device can be told apart.
    generation: u32,
}

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, notify: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        if let Some(data) = unsafe { notify.as_ref() } {
            let level = (data.fMasterVolume.clamp(0.0, 1.0) * 1000.0).round() as usize;
            let mut flags = generation_flags(self.generation);
            if data.bMuted.as_bool() {
                flags |= VOLUME_MUTED;
            }
            self.window.post(msg::VOLUME, level, flags);
        }
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceCallback {
    window: WindowHandle,
}

impl IMMNotificationClient_Impl for DeviceCallback_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        Ok(())
    }

    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eMultimedia {
            self.window.post(msg::AUDIO_DEVICE, 0, 0);
        }
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

pub struct Audio {
    window: WindowHandle,
    enumerator: IMMDeviceEnumerator,
    device_callback: IMMNotificationClient,
    volume_callback: IAudioEndpointVolumeCallback,
    endpoint: Option<IAudioEndpointVolume>,
    generation: u32,
}

impl Audio {
    pub fn new(window: WindowHandle) -> Result<Self> {
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let device_callback: IMMNotificationClient = DeviceCallback { window }.into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&device_callback)? };
        let mut audio = Self {
            window,
            enumerator,
            device_callback,
            volume_callback: VolumeCallback {
                window,
                generation: 0,
            }
            .into(),
            endpoint: None,
            generation: 0,
        };
        audio.attach_default();
        Ok(audio)
    }

    /// Follows the current default playback device. Having no device at all is fine.
    pub fn attach_default(&mut self) {
        self.detach();
        self.generation = self.generation.wrapping_add(1) & 0xff;
        self.volume_callback = VolumeCallback {
            window: self.window,
            generation: self.generation,
        }
        .into();
        let endpoint = unsafe {
            self.enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .and_then(|device| device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None))
        };
        if let Ok(endpoint) = endpoint
            && unsafe { endpoint.RegisterControlChangeNotify(&self.volume_callback) }.is_ok()
        {
            self.endpoint = Some(endpoint);
        }
    }

    /// Name of the default playback device, and whether it's worn (headphones,
    /// headset) rather than a speaker.
    pub fn default_device(&self) -> Option<(String, bool)> {
        // PKEY_Device_FriendlyName and PKEY_AudioEndpoint_FormFactor.
        const FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
            fmtid: windows::core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
            pid: 14,
        };
        const FORM_FACTOR: PROPERTYKEY = PROPERTYKEY {
            fmtid: windows::core::GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e),
            pid: 0,
        };
        const HEADPHONES: u32 = 3;
        const HEADSET: u32 = 5;
        unsafe {
            let device = self
                .enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .ok()?;
            let store = device.OpenPropertyStore(STGM_READ).ok()?;
            let name = store.GetValue(&FRIENDLY_NAME).ok()?.to_string();
            let worn = store
                .GetValue(&FORM_FACTOR)
                .ok()
                .and_then(|value| u32::try_from(&value).ok())
                .is_some_and(|form| form == HEADPHONES || form == HEADSET);
            Some((name, worn))
        }
    }

    /// Generation of the device attached now; see `volume_generation`.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Volume (0 to 1000) and mute state of the default device, if there is one.
    pub fn level(&self) -> Option<(usize, bool)> {
        let endpoint = self.endpoint.as_ref()?;
        unsafe {
            let level = endpoint.GetMasterVolumeLevelScalar().ok()?;
            let muted = endpoint.GetMute().ok()?.as_bool();
            Some(((level.clamp(0.0, 1.0) * 1000.0).round() as usize, muted))
        }
    }

    /// For finding an app's audio sessions on the default device.
    pub fn enumerator(&self) -> &IMMDeviceEnumerator {
        &self.enumerator
    }

    fn detach(&mut self) {
        if let Some(endpoint) = self.endpoint.take() {
            unsafe {
                let _ = endpoint.UnregisterControlChangeNotify(&self.volume_callback);
            }
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.detach();
        unsafe {
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.device_callback);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_generation_should_read_back_the_bits_above_the_flags() {
        let flags = (37 << GENERATION_SHIFT) | VOLUME_OWN | VOLUME_MUTED;
        assert_eq!(volume_generation(flags), 37);
    }
}
