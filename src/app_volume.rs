//! Volume of the app that is playing: the music card's volume bar sets the
//! player's own level, the one Windows' Volume mixer shows next to its name,
//! never the system volume.
//!
//! Windows names the playing app by its app id (`Spotify.exe`, `Chrome`, or a
//! Store id like `SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify`), while audio
//! sessions only name a process, so the two are matched through the process:
//! its package for Store apps, its program's file name otherwise. Core Audio
//! calls the session callbacks on its own threads, so they only post a message
//! to the notch window.

use std::path::Path;

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Foundation::{CloseHandle, HANDLE, S_OK};
use windows::Win32::Media::Audio::{
    AudioSessionDisconnectReason, AudioSessionState, AudioSessionStateExpired,
    IAudioSessionControl, IAudioSessionControl2, IAudioSessionEvents, IAudioSessionEvents_Impl,
    IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume, eMultimedia, eRender,
};
use windows::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
use windows::Win32::System::Com::CLSCTX_ALL;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{BOOL, GUID, Interface, PCWSTR, PWSTR, Result, implement};

use crate::audio::{VOLUME_MUTED, VOLUME_OWN, generation_flags};
use crate::msg::{self, WindowHandle};

/// Tags volume changes BUKU makes itself, so their notification is told apart
/// from changes made in the player or the Volume mixer.
const OWN_CHANGE: GUID = GUID::from_u128(0x4255_4b55_6170_7076_6f6c_0000_0000_0001);

/// `lparam` bit of `msg::APP_VOLUME`: the session ended; look for the player again.
pub const APP_VOLUME_GONE: isize = 4;

#[implement(IAudioSessionEvents)]
struct SessionEvents {
    window: WindowHandle,
    /// Which lookup these sessions came from, so a notification still queued
    /// from the previous player's sessions can be told apart.
    generation: u32,
}

impl IAudioSessionEvents_Impl for SessionEvents_Impl {
    fn OnDisplayNameChanged(&self, _name: &PCWSTR, _context: *const GUID) -> Result<()> {
        Ok(())
    }

    fn OnIconPathChanged(&self, _path: &PCWSTR, _context: *const GUID) -> Result<()> {
        Ok(())
    }

    fn OnSimpleVolumeChanged(&self, level: f32, muted: BOOL, context: *const GUID) -> Result<()> {
        let level = (level.clamp(0.0, 1.0) * 1000.0).round() as usize;
        let mut flags = generation_flags(self.generation);
        if muted.as_bool() {
            flags |= VOLUME_MUTED;
        }
        if unsafe { context.as_ref() } == Some(&OWN_CHANGE) {
            flags |= VOLUME_OWN;
        }
        self.window.post(msg::APP_VOLUME, level, flags);
        Ok(())
    }

    fn OnChannelVolumeChanged(
        &self,
        _count: u32,
        _levels: *const f32,
        _changed: u32,
        _context: *const GUID,
    ) -> Result<()> {
        Ok(())
    }

    fn OnGroupingParamChanged(&self, _grouping: *const GUID, _context: *const GUID) -> Result<()> {
        Ok(())
    }

    fn OnStateChanged(&self, state: AudioSessionState) -> Result<()> {
        if state == AudioSessionStateExpired {
            let flags = generation_flags(self.generation) | APP_VOLUME_GONE;
            self.window.post(msg::APP_VOLUME, 0, flags);
        }
        Ok(())
    }

    fn OnSessionDisconnected(&self, _reason: AudioSessionDisconnectReason) -> Result<()> {
        let flags = generation_flags(self.generation) | APP_VOLUME_GONE;
        self.window.post(msg::APP_VOLUME, 0, flags);
        Ok(())
    }
}

struct Session {
    control: IAudioSessionControl,
    volume: ISimpleAudioVolume,
    events: IAudioSessionEvents,
}

/// The audio sessions of one app on the default playback device. A browser may
/// play through several processes, so a change applies to all of them.
pub struct AppVolume {
    sessions: Vec<Session>,
}

impl AppVolume {
    /// The sessions of the app with this app id, or `None` when it has none
    /// (it hasn't played sound yet, or it can't be matched to a process).
    pub fn find(
        enumerator: &IMMDeviceEnumerator,
        app_id: &str,
        window: WindowHandle,
        generation: u32,
    ) -> Option<Self> {
        if app_id.is_empty() {
            return None;
        }
        let mut sessions = Vec::new();
        unsafe {
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eMultimedia)
                .ok()?;
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None).ok()?;
            let list = manager.GetSessionEnumerator().ok()?;
            for index in 0..list.GetCount().ok()? {
                let Ok(control) = list.GetSession(index) else {
                    continue;
                };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // S_OK means it is the system sounds session; S_FALSE that it isn't
                // (both count as success, so compare exactly).
                if control2.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                let Ok(pid) = control2.GetProcessId() else {
                    continue;
                };
                if control.GetState() == Ok(AudioSessionStateExpired) {
                    continue;
                }
                let Some((exe, package)) = process_identity(pid) else {
                    continue;
                };
                if !app_matches(app_id, &exe, package.as_deref()) {
                    continue;
                }
                let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                let events: IAudioSessionEvents = SessionEvents { window, generation }.into();
                if control.RegisterAudioSessionNotification(&events).is_ok() {
                    sessions.push(Session {
                        control,
                        volume,
                        events,
                    });
                }
            }
        }
        (!sessions.is_empty()).then_some(Self { sessions })
    }

    /// The app's volume (0 to 1000) and mute state, as its first session reports it.
    pub fn level(&self) -> Option<(usize, bool)> {
        self.sessions.iter().find_map(|session| unsafe {
            let level = session.volume.GetMasterVolume().ok()?;
            let muted = session.volume.GetMute().ok()?.as_bool();
            Some(((level.clamp(0.0, 1.0) * 1000.0).round() as usize, muted))
        })
    }

    /// Sets the app's volume (0 to 1). With `unmute`, a level above zero also
    /// turns its mute off, as moving its slider in the Volume mixer does.
    pub fn set_level(&self, level: f32, unmute: bool) {
        let level = level.clamp(0.0, 1.0);
        for session in &self.sessions {
            unsafe {
                // Unmute first, so the level's notification already reads unmuted.
                if unmute
                    && level > 0.0
                    && session.volume.GetMute().is_ok_and(|muted| muted.as_bool())
                {
                    let _ = session.volume.SetMute(false, &OWN_CHANGE);
                }
                let _ = session.volume.SetMasterVolume(level, &OWN_CHANGE);
            }
        }
    }

    pub fn toggle_mute(&self) {
        let Some((_, muted)) = self.level() else {
            return;
        };
        for session in &self.sessions {
            unsafe {
                let _ = session.volume.SetMute(!muted, &OWN_CHANGE);
            }
        }
    }
}

impl Drop for AppVolume {
    fn drop(&mut self) {
        for session in &self.sessions {
            unsafe {
                let _ = session
                    .control
                    .UnregisterAudioSessionNotification(&session.events);
            }
        }
    }
}

/// Full path of a process's program file, and its package family name when it
/// is a Store app.
fn process_identity(pid: u32) -> Option<(String, Option<String>)> {
    unsafe {
        let process: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let exe = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
        .is_ok()
        .then(|| String::from_utf16_lossy(&buffer[..len as usize]));
        // Fails with APPMODEL_ERROR_NO_PACKAGE for ordinary desktop programs.
        let mut family = [0u16; 128];
        let mut family_len = family.len() as u32;
        let package =
            (GetPackageFamilyName(process, &mut family_len, Some(PWSTR(family.as_mut_ptr())))
                == ERROR_SUCCESS)
                .then(|| {
                    // The length counts the terminating null.
                    let end = (family_len as usize).saturating_sub(1).min(family.len());
                    String::from_utf16_lossy(&family[..end])
                });
        let _ = CloseHandle(process);
        Some((exe?, package))
    }
}

/// Whether the program at `exe`, from the Store package `package` if any, is
/// the app Windows calls `app_id`.
///
/// A Store app's id is its package family, `!`, and the app's name
/// (`Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic`), so the package
/// decides, whatever the program file is called. A desktop app's id is usually
/// its file name (`Spotify.exe`) or a name that contains the program's
/// (`Chrome`, `MSEdge`, `Brave`). Very short names are not matched on, so that
/// `a.exe` doesn't match every id with an "a" in it.
fn app_matches(app_id: &str, exe: &str, package: Option<&str>) -> bool {
    let id = app_id.to_lowercase();
    if let (Some((family, _)), Some(package)) = (id.split_once('!'), package)
        && family == package.to_lowercase()
    {
        return true;
    }
    let file = Path::new(exe)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let stem = file.strip_suffix(".exe").unwrap_or(&file);
    if stem.is_empty() {
        return false;
    }
    // The id may be a full path, or just the program's file name.
    let id_file = id.rsplit(['\\', '/']).next().unwrap_or(&id);
    if id_file == file || id_file == stem {
        return true;
    }
    // A Store id names the app after its `!`; only that whole name counts, since
    // words of the package name (`Music`, `Zune`) can be other programs' names.
    // Short names such as `App`, common at the end of Store ids, never count.
    let long_enough = stem.chars().count() >= 4;
    if let Some((_, app)) = id.split_once('!') {
        return long_enough && app == stem;
    }
    long_enough && id.contains(stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_matches_should_pair_common_players_with_their_programs() {
        let pairs = [
            (
                "Spotify.exe",
                r"C:\Users\a\AppData\Roaming\Spotify\Spotify.exe",
            ),
            (
                "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify",
                r"C:\Program Files\WindowsApps\SpotifyAB.SpotifyMusic_1.2.3.4_x64__zpdnekdrzrea0\Spotify.exe",
            ),
            (
                "Chrome",
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            ),
            (
                "MSEdge",
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            ),
            (
                "Brave",
                r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe",
            ),
            (r"C:\Tools\vlc\vlc.exe", r"C:\Tools\vlc\vlc.exe"),
        ];
        assert!(pairs.iter().all(|(id, exe)| app_matches(id, exe, None)));
    }

    #[test]
    fn app_matches_should_pair_a_store_app_by_its_package() {
        let id = "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic";
        let exe = r"C:\Program Files\WindowsApps\Microsoft.ZuneMusic_11.0_x64__8wekyb3d8bbwe\Microsoft.Media.Player.exe";
        assert_eq!(
            [
                app_matches(id, exe, Some("Microsoft.ZuneMusic_8wekyb3d8bbwe")),
                app_matches(id, exe, None),
                app_matches(id, exe, Some("Microsoft.WindowsCalculator_8wekyb3d8bbwe")),
            ],
            [true, false, false]
        );
    }

    #[test]
    fn app_matches_should_not_pair_other_programs() {
        let pairs = [
            (
                "Spotify.exe",
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            ),
            ("Chrome", r"C:\Windows\explorer.exe"),
            (
                "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify",
                r"C:\x\a.exe",
            ),
            ("", r"C:\x\spotify.exe"),
        ];
        assert!(pairs.iter().all(|(id, exe)| !app_matches(id, exe, None)));
    }

    #[test]
    fn app_matches_should_not_pair_a_store_id_with_a_program_named_after_one_of_its_words() {
        let id = "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic";
        let checks =
            [r"C:\Tools\Music.exe", r"C:\Games\Zune.exe"].map(|exe| app_matches(id, exe, None));
        assert_eq!(checks, [false, false]);
    }

    #[test]
    fn app_matches_should_not_pair_a_generic_store_app_name_with_a_program_of_that_name() {
        // Many Store ids end in a generic name such as `!App`.
        let id = "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App";
        assert!(!app_matches(id, r"C:\Dev\app.exe", None));
    }
}
