<p align="center">
  <img src="branding/buku-logo-transparent.png" width="180" alt="BUKU">
</p>

<h1 align="center">BUKU</h1>

<p align="center">
  A little notch for the top of your Windows screen.<br>
  Volume, brightness and Caps Lock pop-ups, music with synced lyrics, a focus
  timer, water reminders, tasks and a file shelf.
</p>

## Features

- **Volume, brightness and Caps Lock** pop up in the notch when they change.
- **Now playing**: hover the notch to see the current track and its album art
  from any app that reports to Windows (Spotify, browsers, media players), with
  previous, play/pause and next.
  - A volume bar under them sets the playing app's own volume, the one the
    Windows Volume mixer shows next to it. Click or drag to set it, click the
    speaker icon to mute the app, or use the mouse wheel when Windows' "Scroll
    inactive windows" setting is on.
  - The bar shows once the app is making sound and can be matched to its audio.
    Firefox can't be matched, so it has none.
- **Synced lyrics** from [LRCLIB](https://lrclib.net): the current line shows
  inside the notch while music plays, and on the media card. Each line appears a
  moment before it is sung (1 second by default, adjustable). Video titles such
  as "Artist - Song (Official Video)" are cleaned before the lookup.
  - Lyrics for another cut of a song (a video with a longer intro, say) can run
    early or late. On the media card, click the lyric line the moment it is sung,
    or use the arrows beside it to move a line back or forward to now. BUKU keeps
    that timing for the song; click the offset under the bar to reset it.
- **Focus timer**: a Pomodoro, 25 minutes of focus and then a 5-minute break by
  default. The countdown shows in the notch while it runs, with a chime when
  each part ends.
- **Water**: count glasses through the day toward a goal (8 by default), with a
  gentle reminder after an hour without one, between 9:00 and 22:00. The count
  resets at midnight.
- **Tasks**: a short to-do list. Add a task from the card: a small text box opens
  under the notch (Enter saves, Esc cancels). Click a task to tick it off.
- **Shelf**: drag files onto the notch to keep them at hand. Click one to open
  it, or drag it back out into any folder or app. Only the file's location is
  remembered; nothing is copied. Programs and scripts are never run from the
  Shelf: clicking one opens its folder instead.
- **Notices** when you plug in or unplug a laptop, when the battery drops to 20%
  and 10%, when the Wi-Fi network changes, when the sound output switches (for
  example to headphones), and when the keyboard language changes.

The notch hides itself while a full-screen app (a game, a full-screen video) is
in front, and comes back when it closes. Turn on **Show over full screen** to
keep it on top instead; games that take the screen over in exclusive mode still
cover it. BUKU sleeps when nothing changes: no
polling, no timers and no frames while the notch is still.

## Requirements

64-bit Windows 10 (version 1809 or later) or Windows 11.

## Installing

Run `BUKU-Setup-<version>.exe`. It installs for the current user only and needs
no administrator rights. Remove BUKU from **Settings > Apps > Installed apps**,
like any other app.

## Using BUKU

- Hover the notch to open it, or click it to open it at once. The tabs at the
  bottom switch between Music, Focus, Water, Tasks and Shelf.
- Right-click the notch, or click the tray icon, for **Settings…** and **Exit**.

### Settings

Changes apply at once; there is no Save button.

| Setting | What it does |
|---|---|
| Start with Windows | Opens BUKU when you sign in. |
| Caps Lock key | **Normal**; **Ctrl+Shift+L**, sent once per press; or **Custom**, any key or combination you record. |
| Notch screen | The screen the notch sits on, when more than one is connected. |
| Show over full screen | Keeps the notch on top of full-screen videos and games instead of hiding it. |
| Accent colour | The colour of switches, the music bars and highlights: white (the default), yellow, orange, red, pink, purple, blue, cyan or green. |
| Lyrics in the notch | Shows the current line inside the notch while music plays. |
| Download lyrics | Looks lyrics up on lrclib.net. Off means BUKU makes no network requests at all. |
| Show lyrics early by | How long before it is sung each line appears: 0 to 3 seconds. |
| Focus length, Break length | The two parts of the focus timer. |
| Water reminders, Glasses a day | The hourly reminder and the daily goal. |
| Browse app files | Opens the folder BUKU is installed in. |
| Clear cache | Removes lyrics lookups that earlier versions of BUKU left in the Windows web cache, and says how many it removed. Nothing else in the cache is touched. |

**Custom Caps Lock key**: choose **Custom** with the arrows and recording starts.
Press the key you want (for example Esc or F13), or hold modifiers and press a
key (for example Ctrl+Alt+K). A single key is held down for as long as Caps Lock
is; a combination is sent once per press. Click the row's text to record a new
one. Caps Lock itself and the Windows key can't be used.

**More than one screen**: besides **Notch screen**, you can drag the notch
(press on it away from its buttons) toward another screen and let go. It stays
there after a restart, and goes back to the main screen if that screen is
disconnected.

### Where BUKU keeps its data

Everything stays on your computer, in `%APPDATA%\BUKU\`:

| File | Contents |
|---|---|
| `settings.json` | Your settings |
| `water.json` | The day's water count |
| `tasks.json` | Your tasks |
| `shelf.json` | The locations of the files on the Shelf |
| `lyric_sync.json` | Lyric timings you set, stored by a hash of title, artist and length, never by name |

## Privacy

BUKU makes one kind of network request: looking up lyrics on lrclib.net when a
new track starts, sending only its title, artist, album and length. Nothing is
kept on disk: no web cache and no cookies. Turn off **Download lyrics** and BUKU
makes no requests at all.

Caps Lock is observed through Windows raw input, and every other key is ignored
the moment it arrives. Only a Caps Lock mode other than Normal installs a
keyboard hook, and it changes Caps Lock alone. The details are in
[docs/PRIVACY.md](docs/PRIVACY.md).

## Notes

- Brightness changes show when Windows' brightness setting changes (the
  brightness keys on most laptops, the Windows slider). External monitors
  adjusted with their own buttons don't tell Windows, so those can't be shown.

## License

MIT. See [LICENSE](LICENSE).
