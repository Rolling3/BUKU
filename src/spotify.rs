//! Shortcuts to Spotify playlists, albums, songs, artists and podcasts, stored
//! in `%APPDATA%\BUKU\spotify.json`.
//!
//! A shortcut is a link copied in Spotify (Share, Copy link). Clicking it opens
//! that page in the Spotify app, or on open.spotify.com when the app isn't
//! installed. BUKU never signs in to Spotify and keeps nothing but the kind,
//! the id and the name of each shortcut. With "Name Spotify shortcuts" on, the
//! name comes from Spotify's public oEmbed endpoint, once, when one is added.

use std::sync::{Arc, Mutex};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use windows::Win32::UI::Shell::{
    ASSOCF_IS_PROTOCOL, ASSOCSTR_APPID, AssocQueryStringW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PWSTR, w};

use crate::msg::{self, WindowHandle};
use crate::{lyrics, store, tasks};

const FILE: &str = "spotify.json";
/// What the Spotify page shows: three rows of three, with Add in the last spot.
pub const MAX_SHORTCUTS: usize = 8;
pub const MAX_NAME_CHARS: usize = 60;
/// Room for a link with its tracking query and a country prefix.
pub const MAX_LINK_CHARS: usize = 300;
/// Spotify ids are 22 base-62 characters.
const ID_LEN: usize = 22;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Playlist,
    Album,
    Track,
    Artist,
    Show,
    Episode,
}

impl Kind {
    const ALL: [Kind; 6] = [
        Kind::Playlist,
        Kind::Album,
        Kind::Track,
        Kind::Artist,
        Kind::Show,
        Kind::Episode,
    ];

    /// The word Spotify uses for it in links and URIs.
    fn word(self) -> &'static str {
        match self {
            Kind::Playlist => "playlist",
            Kind::Album => "album",
            Kind::Track => "track",
            Kind::Artist => "artist",
            Kind::Show => "show",
            Kind::Episode => "episode",
        }
    }

    fn from_word(word: &str) -> Option<Kind> {
        Kind::ALL
            .into_iter()
            .find(|kind| kind.word().eq_ignore_ascii_case(word))
    }

    /// Shown in place of a name until one is known.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Playlist => "Playlist",
            Kind::Album => "Album",
            Kind::Track => "Song",
            Kind::Artist => "Artist",
            Kind::Show => "Podcast",
            Kind::Episode => "Episode",
        }
    }
}

/// A page on Spotify: what kind it is and its id. Built only by `parse`, so the
/// id is always 22 letters and digits and the URI made from it can't carry
/// anything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    kind: Kind,
    id: String,
}

fn is_id(id: &str) -> bool {
    id.len() == ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

impl Link {
    /// Reads a link as Spotify's Share menu copies it
    /// (`https://open.spotify.com/playlist/<id>?si=...`, also with an
    /// `intl-xx/` country part or `embed/`) or a Spotify URI
    /// (`spotify:playlist:<id>`). Anything else, short `spotify.link` links
    /// included (they need a network lookup to resolve), is `None`.
    pub fn parse(text: &str) -> Option<Link> {
        let text = text.trim();
        if text.chars().count() > MAX_LINK_CHARS {
            return None;
        }
        let (kind, id) = match strip_prefix_ignore_case(text, "spotify:") {
            Some(rest) => {
                let parts: Vec<&str> = rest.split(':').collect();
                match parts.as_slice() {
                    // Older playlist URIs name their owner first.
                    [kind, id] | ["user", _, kind, id] => (*kind, *id),
                    _ => return None,
                }
            }
            None => {
                let rest = strip_prefix_ignore_case(text, "https://")
                    .or_else(|| strip_prefix_ignore_case(text, "http://"))
                    .unwrap_or(text);
                let rest = rest.split(['?', '#']).next().unwrap_or_default();
                let mut parts = rest.split('/');
                let host = parts.next()?;
                if !host.eq_ignore_ascii_case("open.spotify.com") {
                    return None;
                }
                let mut path = parts
                    .filter(|part| !part.is_empty())
                    .skip_while(|part| part.starts_with("intl-") || *part == "embed");
                let kind = path.next()?;
                let id = path.next()?;
                if path.next().is_some() {
                    return None;
                }
                (kind, id)
            }
        };
        let kind = Kind::from_word(kind)?;
        is_id(id).then(|| Link {
            kind,
            id: id.to_owned(),
        })
    }

    /// Opens the page in the Spotify app.
    fn uri(&self) -> String {
        format!("spotify:{}:{}", self.kind.word(), self.id)
    }

    /// The same page on the web.
    fn web(&self) -> String {
        format!("https://open.spotify.com/{}/{}", self.kind.word(), self.id)
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// A name as it's kept and shown: one line, no control or direction characters,
/// at most `MAX_NAME_CHARS` characters.
pub fn clean_name(name: &str) -> String {
    tasks::clean_line(name, MAX_NAME_CHARS)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Shortcut {
    /// Empty until known; the kind's label shows instead.
    pub name: String,
    kind: Kind,
    id: String,
}

impl Shortcut {
    pub fn link(&self) -> Link {
        Link {
            kind: self.kind,
            id: self.id.clone(),
        }
    }

    pub fn shown_name(&self) -> &str {
        if self.name.is_empty() {
            self.kind.label()
        } else {
            &self.name
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Shortcuts {
    shortcuts: Vec<Shortcut>,
}

/// Read one by one: an entry that can't be used (an id that isn't one, a kind
/// from a newer version) is skipped rather than losing the others.
impl<'de> Deserialize<'de> for Shortcuts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let mut list = Shortcuts::default();
        let items = value.get("shortcuts").and_then(Value::as_array);
        for item in items.into_iter().flatten() {
            let kind = item
                .get("kind")
                .and_then(Value::as_str)
                .and_then(Kind::from_word);
            let id = item
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| is_id(id));
            let (Some(kind), Some(id)) = (kind, id) else {
                continue;
            };
            let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
            let link = Link {
                kind,
                id: id.to_owned(),
            };
            if !list.contains(&link) && list.shortcuts.len() < MAX_SHORTCUTS {
                list.shortcuts.push(Shortcut {
                    name: clean_name(name),
                    kind,
                    id: link.id,
                });
            }
        }
        Ok(list)
    }
}

impl Shortcuts {
    /// A list in memory only, for tests: `add` would save it over the real one.
    #[cfg(test)]
    pub fn unnamed(links: &[Link]) -> Self {
        Self {
            shortcuts: links
                .iter()
                .map(|link| Shortcut {
                    name: String::new(),
                    kind: link.kind,
                    id: link.id.clone(),
                })
                .collect(),
        }
    }

    pub fn load() -> Self {
        store::load(FILE)
    }

    fn save(&self) {
        // Tests change lists in memory; they must never reach the user's file.
        if cfg!(test) {
            return;
        }
        // A failed save keeps the list in memory; nothing is worth an error box here.
        let _ = store::save(FILE, self);
    }

    pub fn items(&self) -> &[Shortcut] {
        &self.shortcuts
    }

    pub fn is_full(&self) -> bool {
        self.shortcuts.len() >= MAX_SHORTCUTS
    }

    pub fn contains(&self, link: &Link) -> bool {
        self.shortcuts
            .iter()
            .any(|s| s.kind == link.kind && s.id == link.id)
    }

    /// Adds a shortcut at the end. False when the list is full or already has it.
    pub fn add(&mut self, link: &Link, name: &str) -> bool {
        if self.is_full() || self.contains(link) {
            return false;
        }
        self.shortcuts.push(Shortcut {
            name: clean_name(name),
            kind: link.kind,
            id: link.id.clone(),
        });
        self.save();
        true
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.shortcuts.len() {
            self.shortcuts.remove(index);
            self.save();
        }
    }

    /// Gives a shortcut that has no name yet the one Spotify sent. One named in
    /// the meantime keeps its name.
    pub fn name_if_unnamed(&mut self, link: &Link, name: &str) -> bool {
        let name = clean_name(name);
        let Some(shortcut) = self
            .shortcuts
            .iter_mut()
            .find(|s| s.kind == link.kind && s.id == link.id && s.name.is_empty())
        else {
            return false;
        };
        if name.is_empty() {
            return false;
        }
        shortcut.name = name;
        self.save();
        true
    }
}

/// Whether something on this PC opens `spotify:` links: the Spotify app, from
/// the Microsoft Store or from Spotify's own installer.
fn app_installed() -> bool {
    let mut buffer = [0u16; 512];
    let mut len = buffer.len() as u32;
    unsafe {
        AssocQueryStringW(
            ASSOCF_IS_PROTOCOL,
            ASSOCSTR_APPID,
            w!("spotify"),
            None,
            Some(PWSTR(buffer.as_mut_ptr())),
            &mut len,
        )
        .is_ok()
    }
}

/// Opens the shortcut's page in the Spotify app, or in the browser without it.
pub fn open(link: &Link) {
    let target = if app_installed() {
        link.uri()
    } else {
        link.web()
    };
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(target),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Names Spotify sent, waiting for the notch to pick them up.
pub type Names = Arc<Mutex<Vec<(Link, String)>>>;

/// Asks Spotify for the page's name on a thread of its own; the notch hears
/// `msg::SPOTIFY_NAME` when it arrives. Nothing comes back when it can't be had.
pub fn look_up_name(window: WindowHandle, link: Link, names: Names) {
    let _ = std::thread::Builder::new()
        .name("spotify name".into())
        .spawn(move || {
            unsafe {
                let _ = windows::Win32::System::WinRT::RoInitialize(
                    windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
                );
            }
            if let Some(name) = fetch_name(&link) {
                if let Ok(mut names) = names.lock() {
                    names.push((link, name));
                }
                window.post(msg::SPOTIFY_NAME, 0, 0);
            }
        });
}

/// Spotify's oEmbed answer; only the title is used.
#[derive(Deserialize)]
struct Embed {
    title: String,
}

fn fetch_name(link: &Link) -> Option<String> {
    let client = lyrics::private_client().ok()?;
    let url = format!(
        "https://open.spotify.com/oembed?url={}",
        lyrics::escape(&link.web()).ok()?
    );
    let body = lyrics::get_text(&client, &url).ok()??;
    name_from_embed(&body)
}

fn name_from_embed(body: &str) -> Option<String> {
    let embed: Embed = serde_json::from_str(body).ok()?;
    let name = clean_name(&embed.title);
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "37i9dQZF1DXcBWIGoYBM5M";

    fn link(kind: Kind) -> Link {
        Link {
            kind,
            id: ID.into(),
        }
    }

    #[test]
    fn a_shared_link_should_be_read_with_or_without_its_extras() {
        for text in [
            format!("https://open.spotify.com/playlist/{ID}"),
            format!("https://open.spotify.com/playlist/{ID}?si=abc123&pt=x"),
            format!("  https://open.spotify.com/intl-ar/playlist/{ID}/  "),
            format!("open.spotify.com/embed/playlist/{ID}#top"),
            format!("HTTPS://OPEN.SPOTIFY.COM/playlist/{ID}"),
            format!("spotify:playlist:{ID}"),
            format!("spotify:user:someone:playlist:{ID}"),
        ] {
            assert_eq!(Link::parse(&text), Some(link(Kind::Playlist)), "{text}");
        }
    }

    #[test]
    fn every_kind_should_be_read() {
        for kind in Kind::ALL {
            let text = format!("https://open.spotify.com/{}/{ID}", kind.word());
            assert_eq!(Link::parse(&text).map(|l| l.kind), Some(kind));
        }
    }

    #[test]
    fn links_that_are_not_spotify_pages_should_be_refused() {
        for text in [
            String::new(),
            "hello".into(),
            format!("https://open.spotify.com.evil.example/playlist/{ID}"),
            format!("https://evil.example/open.spotify.com/playlist/{ID}"),
            format!("https://user@open.spotify.com/playlist/{ID}"),
            format!("https://open.spotify.com/user/{ID}"),
            format!("https://open.spotify.com/playlist/{ID}/extra"),
            "https://open.spotify.com/playlist/short".into(),
            format!("https://open.spotify.com/playlist/{ID}x"),
            "https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5-".into(),
            "https://spotify.link/AbCdEf".into(),
            format!("spotify:playlist:{ID}:extra"),
            format!("spotify:playlist:{ID}\" --flag"),
            format!("file:///open.spotify.com/playlist/{ID}"),
            format!(
                "https://open.spotify.com/playlist/{}",
                "\u{0661}".repeat(22)
            ),
            format!("https://open.spotify.com/playlist/{ID}?{}", "x".repeat(400)),
        ] {
            assert_eq!(Link::parse(&text), None, "{text}");
        }
    }

    #[test]
    fn a_link_should_open_as_the_page_it_names() {
        let link = link(Kind::Track);
        assert_eq!(link.uri(), format!("spotify:track:{ID}"));
        assert_eq!(link.web(), format!("https://open.spotify.com/track/{ID}"));
    }

    #[test]
    fn an_unnamed_shortcut_should_show_its_kind() {
        let mut list = Shortcuts::default();
        list.shortcuts.push(Shortcut {
            name: String::new(),
            kind: Kind::Show,
            id: ID.into(),
        });
        assert_eq!(list.items()[0].shown_name(), "Podcast");
    }

    #[test]
    fn the_list_should_stop_at_eight_and_skip_repeats() {
        let mut list = Shortcuts::default();
        let ids: Vec<String> = (0..10).map(|i| format!("{i}{}", &ID[1..])).collect();
        for id in &ids {
            let link = Link {
                kind: Kind::Album,
                id: id.clone(),
            };
            if !list.is_full() && !list.contains(&link) {
                list.shortcuts.push(Shortcut {
                    name: String::new(),
                    kind: link.kind,
                    id: link.id,
                });
            }
        }
        assert_eq!(list.items().len(), MAX_SHORTCUTS);
        let first = Link {
            kind: Kind::Album,
            id: ids[0].clone(),
        };
        assert!(list.contains(&first));
        assert!(!list.contains(&Link {
            kind: Kind::Playlist,
            id: ids[0].clone(),
        }));
    }

    #[test]
    fn a_name_from_spotify_should_not_replace_one_given_meanwhile() {
        let mut list = Shortcuts::default();
        list.shortcuts.push(Shortcut {
            name: "Mine".into(),
            kind: Kind::Playlist,
            id: ID.into(),
        });
        // `name_if_unnamed` saves on success; this one must not get that far.
        assert!(!list.name_if_unnamed(&link(Kind::Playlist), "Theirs"));
        assert_eq!(list.items()[0].name, "Mine");
    }

    #[test]
    fn a_saved_list_should_drop_entries_it_cannot_use() {
        let json = serde_json::json!({"shortcuts": [
            {"name": "Car\u{202E} songs", "kind": "playlist", "id": ID},
            {"name": "Again", "kind": "playlist", "id": ID},
            {"name": "Bad id", "kind": "album", "id": "spotify:album:x"},
            {"name": "New kind", "kind": "audiobook", "id": ID},
            {"kind": "track", "id": ID}
        ]});
        let list: Shortcuts = serde_json::from_value(json).unwrap_or_default();
        let names: Vec<&str> = list.items().iter().map(Shortcut::shown_name).collect();
        assert_eq!(names, ["Car songs", "Song"]);
    }

    #[test]
    fn a_saved_list_should_keep_at_most_eight() {
        let entries: Vec<String> = (0..12)
            .map(|i| format!(r#"{{"kind":"album","id":"{:02}{}"}}"#, i, &ID[2..]))
            .collect();
        let json = format!(r#"{{"shortcuts":[{}]}}"#, entries.join(","));
        let list: Shortcuts = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(list.items().len(), MAX_SHORTCUTS);
    }

    #[test]
    fn spotifys_answer_should_give_a_clean_name() {
        let body = serde_json::json!({
            "html": "<iframe>",
            "title": "Today\u{2019}s Top Hits\n\u{2066}x\u{2069}",
            "type": "rich"
        })
        .to_string();
        let body = body.as_str();
        assert_eq!(
            name_from_embed(body).as_deref(),
            Some("Today\u{2019}s Top Hits x")
        );
        assert_eq!(name_from_embed(r#"{"title":"   "}"#), None);
        assert_eq!(name_from_embed("<html>busy</html>"), None);
        let long = format!(r#"{{"title":"{}"}}"#, "a".repeat(500));
        assert_eq!(
            name_from_embed(&long).map(|n| n.chars().count()),
            Some(MAX_NAME_CHARS)
        );
    }
}
