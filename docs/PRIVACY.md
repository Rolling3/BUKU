# Privacy

## Network

BUKU makes exactly one kind of network request: looking up lyrics on
`lrclib.net` when a new track starts playing. The request contains:

- track title
- artist
- album
- track length in seconds

Nothing else is sent: no identifiers, no usage data, no crash reports. The
request carries the header `User-Agent: BUKU/<version>`.

Lyrics are cached in memory for the session only. The lookup keeps nothing on
disk: it bypasses Windows' web cache, sends and stores no cookies, and follows
no redirects. Versions before this stored each lookup in that cache;
**Clear cache** in Settings removes those `lrclib.net` entries and nothing else.
Windows can keep traces in the cache database's own log files until it
replaces them. Turn off
**Download lyrics** in Settings and BUKU makes no network
requests at all.

## Keyboard

To show the Caps Lock indicator BUKU registers for raw keyboard input. Windows
then tells BUKU about key presses **after** delivering them to the app you are
typing in. BUKU:

- cannot block, delay or change any key;
- looks only at Caps Lock, and at Shift, Alt, Ctrl, Win and Space being
  released, after which it reads the current keyboard language; every other key
  is dropped at once;
- stores and sends nothing about your typing.

### Caps Lock remapping

Only if you choose a Caps Lock mode other than **Normal** in Settings, BUKU
installs a keyboard hook so it can replace that one key (with Ctrl+Shift+L, or
the key or keys you record in Settings, which are saved in settings.json). The
hook acts on Caps Lock alone, passes every other
key on immediately and unchanged, and records nothing. Setting the mode back to
Normal removes the hook.

## Other data

- Volume and brightness levels are read from Windows notifications and only
  drawn on screen.
- Media information (title, artist, position) is read from Windows' media
  session API and only drawn on screen, apart from the lyrics lookup above.
- Settings, the day's water count and your tasks are stored in `%APPDATA%\BUKU\`
  (`settings.json`, `water.json`, `tasks.json`, `shelf.json`) and never leave
  your computer. The Shelf stores file locations only, never file contents.
- Lyric timings you set on the media card are stored in
  `%APPDATA%\BUKU\lyric_sync.json` as a hash of each song's title, artist and length
  with its offset, never the song's name, and never leave your computer.
- Battery level, power source, the network name and the sound output's name are
  read from Windows when they change and only drawn on screen.
- "Start with Windows" adds a `BUKU` value under
  `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, only when
  you turn it on, and removes it when you turn it off.
