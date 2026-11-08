//! A text viewer's state over a manual (#1856): the page or the contents on
//! show, its scroll, the selected link and the Back/Forward history. The
//! terminal editors' F1 overlay maps its keys to [`Key`] and draws
//! [`Reader::view`]; nothing here knows about a terminal.

use crate::md::Link;
use crate::text::{self, Laid, Line, LinkAt, Look, Span};
use crate::{Manual, Page};
use std::sync::Arc;

/// What a key asks of the reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    /// Select the next link on the page (Tab), wrapping.
    NextLink,
    /// Select the previous link (Shift+Tab), wrapping.
    PrevLink,
    /// Follow the selected link (Enter).
    Follow,
    Back,
    Forward,
    Contents,
    /// The manual's root page (F1 inside the reader).
    Root,
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
}

/// What is on show: a page (by id) or the contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Page(String),
    Contents,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Place {
    shown: Shown,
    scroll: usize,
}

/// The hints the footer shows when no link is selected.
pub const HINTS: &str =
    "↑↓ PgUp PgDn scroll · Tab link · Enter go · Bksp back · c contents · F1 start · Esc close";

/// What the host draws.
#[derive(Clone, Copy, Debug)]
pub struct View<'a> {
    /// The visible lines, from the scroll.
    pub lines: &'a [Line],
    /// The selected link's index (a span's `look.link`).
    pub selected: Option<usize>,
    pub footer: &'a str,
}

#[derive(Debug)]
pub struct Reader {
    manual: Arc<Manual>,
    shown: Shown,
    scroll: usize,
    selected: Option<usize>,
    back: Vec<Place>,
    forward: Vec<Place>,
    /// The last layout and the width it was laid at.
    laid: Option<(usize, Laid)>,
    height: usize,
    /// A heading to scroll to at the next layout (`page#anchor`).
    anchor: Option<String>,
    /// The footer's text: a note (an external link's address) or the hints.
    footer: String,
}

impl Reader {
    /// A reader on `page` (at its `anchor` heading), else on the root. `None`
    /// for a manual without pages.
    pub fn open(manual: Arc<Manual>, page: &str, anchor: Option<&str>) -> Option<Reader> {
        let id = match manual.page(page) {
            Some(p) => p.id.clone(),
            None => root(&manual)?.id.clone(),
        };
        Some(Reader {
            manual,
            shown: Shown::Page(id),
            scroll: 0,
            selected: None,
            back: Vec::new(),
            forward: Vec::new(),
            laid: None,
            height: 1,
            anchor: anchor.map(str::to_string),
            footer: HINTS.to_string(),
        })
    }

    pub fn manual(&self) -> &Manual {
        &self.manual
    }

    pub fn shown(&self) -> &Shown {
        &self.shown
    }

    /// The page on show; `None` on the contents.
    pub fn page_id(&self) -> Option<&str> {
        match &self.shown {
            Shown::Page(id) => Some(id),
            Shown::Contents => None,
        }
    }

    /// The page's title, or `Contents`.
    pub fn title(&self) -> &str {
        match &self.shown {
            Shown::Page(id) => self.manual.page(id).map_or(id.as_str(), |p| &p.title),
            Shown::Contents => "Contents",
        }
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Lay the shown page out for a view `width` columns wide and `height`
    /// lines high (each at least 1), again only when the width or the page
    /// changed, and keep the scroll in range. Call before drawing.
    pub fn layout(&mut self, width: usize, height: usize, measure: &dyn Fn(&str) -> usize) {
        let width = width.max(1);
        self.height = height.max(1);
        if self.laid.as_ref().is_none_or(|(w, _)| *w != width) {
            let laid = match &self.shown {
                Shown::Page(id) => match self.manual.page(id) {
                    Some(p) => text::lay(p, width, measure),
                    None => Laid::default(),
                },
                Shown::Contents => contents(&self.manual),
            };
            self.laid = Some((width, laid));
        }
        if let Some(anchor) = self.anchor.take() {
            if let Some(line) = self.laid().and_then(|l| l.anchor(&anchor)) {
                self.scroll = line;
            }
        }
        self.clamp();
    }

    /// What to draw: the lines from the scroll, the selected link, the
    /// footer. Empty before the first [`layout`](Reader::layout).
    pub fn view(&self) -> View<'_> {
        let lines = self.laid().map_or(&[][..], |l| &l.lines[..]);
        let from = self.scroll.min(lines.len());
        View {
            lines: &lines[from..],
            selected: self.selected,
            footer: &self.footer,
        }
    }

    /// Scroll by `lines` (the mouse wheel; negative is up).
    pub fn wheel(&mut self, lines: isize) {
        self.scroll = self.scroll.saturating_add_signed(lines);
        self.clamp();
    }

    pub fn key(&mut self, key: Key) -> Outcome {
        let step = self.height.saturating_sub(1).max(1);
        match key {
            Key::Up => self.scroll = self.scroll.saturating_sub(1),
            Key::Down => self.scroll = self.scroll.saturating_add(1),
            Key::PageUp => self.scroll = self.scroll.saturating_sub(step),
            Key::PageDown => self.scroll = self.scroll.saturating_add(step),
            Key::Home => self.scroll = 0,
            // The largest scroll: the next layout clamps it to the bottom,
            // and the keys before that saturate (#1856 r1).
            Key::End => self.scroll = usize::MAX,
            Key::NextLink => self.step_link(true),
            Key::PrevLink => self.step_link(false),
            Key::Follow => self.follow(),
            Key::Back => self.walk(true),
            Key::Forward => self.walk(false),
            Key::Contents => {
                if self.shown != Shown::Contents {
                    self.go(Shown::Contents, None);
                }
            }
            Key::Root => {
                if let Some(id) = root(&self.manual).map(|p| p.id.clone()) {
                    self.go(Shown::Page(id), None);
                }
            }
            Key::Close => return Outcome::Close,
        }
        self.clamp();
        Outcome::Stay
    }

    fn laid(&self) -> Option<&Laid> {
        self.laid.as_ref().map(|(_, l)| l)
    }

    fn links(&self) -> &[LinkAt] {
        self.laid().map_or(&[], |l| &l.links)
    }

    fn clamp(&mut self) {
        if let Some(l) = self.laid() {
            let max = l.lines.len().saturating_sub(self.height);
            self.scroll = self.scroll.min(max);
        }
    }

    fn step_link(&mut self, next: bool) {
        let n = self.links().len();
        if n == 0 {
            return;
        }
        let (top, bottom) = (self.scroll, self.scroll.saturating_add(self.height));
        let at = match self.selected {
            Some(i) if next => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            // Nothing selected: the first link starting at or below the top
            // of the view, even one below the screen, else the page's first;
            // going back, the last link starting above the bottom of the
            // view, else the page's last.
            None if next => self.links().iter().position(|l| l.line >= top).unwrap_or(0),
            None => self
                .links()
                .iter()
                .rposition(|l| l.line < bottom)
                .unwrap_or(n - 1),
        };
        self.selected = Some(at);
        let line = self.links()[at].line;
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll.saturating_add(self.height) {
            self.scroll = line + 1 - self.height;
        }
        self.footer = self.describe(at);
    }

    /// The footer for the selected link: where it goes.
    fn describe(&self, at: usize) -> String {
        match &self.links()[at].link {
            Link::Page { page, anchor } => {
                let title = self.manual.page(page).map_or(page.as_str(), |p| &p.title);
                match anchor {
                    Some(a) => format!("Enter: {title} › #{a}"),
                    None => format!("Enter: {title}"),
                }
            }
            Link::Anchor(a) => format!("Enter: #{a}"),
            Link::External(url) => format!("Leaves the manual: {url}"),
            Link::Other(t) => format!("Broken link: {t}"),
        }
    }

    fn follow(&mut self) {
        let Some(at) = self.selected else {
            return;
        };
        let Some(link) = self.links().get(at).map(|l| l.link.clone()) else {
            return;
        };
        match link {
            Link::Page { page, anchor } => {
                if self.manual.page(&page).is_some() {
                    self.go(Shown::Page(page), anchor);
                }
            }
            Link::Anchor(a) => {
                if let Some(line) = self.laid().and_then(|l| l.anchor(&a)) {
                    self.remember();
                    self.scroll = line;
                }
            }
            // A terminal opens nothing: the footer shows where it goes.
            Link::External(url) => self.footer = format!("Open in a browser: {url}"),
            Link::Other(_) => {}
        }
    }

    /// Push where the reader is now onto Back, clearing Forward.
    fn remember(&mut self) {
        self.back.push(Place {
            shown: self.shown.clone(),
            scroll: self.scroll,
        });
        self.forward.clear();
    }

    fn go(&mut self, shown: Shown, anchor: Option<String>) {
        self.remember();
        self.show(Place { shown, scroll: 0 });
        self.anchor = anchor;
    }

    fn show(&mut self, place: Place) {
        if place.shown != self.shown {
            self.laid = None;
        }
        self.shown = place.shown;
        self.scroll = place.scroll;
        self.selected = None;
        self.anchor = None;
        self.footer = HINTS.to_string();
    }

    fn walk(&mut self, back: bool) {
        let (from, to) = if back {
            (&mut self.back, &mut self.forward)
        } else {
            (&mut self.forward, &mut self.back)
        };
        let Some(place) = from.pop() else {
            return;
        };
        to.push(Place {
            shown: self.shown.clone(),
            scroll: self.scroll,
        });
        self.show(place);
    }
}

/// The manual's root page: the contents' first entry, else its first page.
fn root(manual: &Manual) -> Option<&Page> {
    manual
        .contents
        .first()
        .and_then(|e| manual.page(&e.id))
        .or_else(|| manual.pages.first())
}

/// The contents as a page: one line per entry, two spaces of indent per
/// depth, each a link to its page.
fn contents(manual: &Manual) -> Laid {
    let mut laid = Laid::default();
    for (n, e) in manual.contents.iter().enumerate() {
        let title = manual.page(&e.id).map_or(e.id.as_str(), |p| &p.title);
        laid.lines.push(Line {
            spans: vec![
                Span {
                    text: "  ".repeat(e.depth),
                    look: Look::default(),
                },
                Span {
                    text: title.to_string(),
                    look: Look {
                        link: Some(n),
                        ..Look::default()
                    },
                },
            ],
        });
        laid.links.push(LinkAt {
            link: Link::Page {
                page: e.id.clone(),
                anchor: None,
            },
            line: n,
        });
    }
    laid
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn cols(s: &str) -> usize {
        s.chars().count()
    }

    fn word() -> Arc<Manual> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixture/word");
        Arc::new(Manual::load(&dir).unwrap())
    }

    fn manual(pages: &[(&str, &str)]) -> Arc<Manual> {
        let contents: String = pages.iter().map(|(id, _)| format!("{id}\n")).collect();
        let mut files = vec![("contents.txt".to_string(), contents)];
        files.extend(
            pages
                .iter()
                .map(|(id, src)| (format!("{id}.md"), src.to_string())),
        );
        Arc::new(Manual::from_files(files))
    }

    fn reader(m: &Arc<Manual>, page: &str, height: usize) -> Reader {
        let mut r = Reader::open(m.clone(), page, None).unwrap();
        r.layout(40, height, &cols);
        r
    }

    fn top(r: &Reader) -> String {
        r.view().lines.first().map(Line::text).unwrap_or_default()
    }

    fn long() -> Arc<Manual> {
        let body: String = (0..30).map(|i| format!("Line {i}.\n\n")).collect();
        let src = format!(
            "---\ntitle: Long\n---\n{body}See [one](#end) and [two](other#far) and [web](https://example.org) and [bad](no!thing).\n\n# End\n\nLast."
        );
        manual(&[
            ("long", &src),
            ("other", "---\ntitle: Other\n---\nTop.\n\n# Far\n\nThere."),
        ])
    }

    #[test]
    fn opens_on_a_page_or_the_root() {
        let m = word();
        let r = reader(&m, "home", 10);
        assert_eq!(r.page_id(), Some("home"));
        assert_eq!(r.title(), "Home tab");
        assert_eq!(top(&r), "Home tab");
        let r = reader(&m, "no-such-page", 10);
        assert_eq!(r.page_id(), Some("welcome"));
        assert!(Reader::open(Arc::new(Manual::default()), "home", None).is_none());
    }

    #[test]
    fn scrolling_is_clamped() {
        let m = long();
        let mut r = reader(&m, "long", 10);
        let lines = r.laid().unwrap().lines.len();
        r.key(Key::Up);
        assert_eq!(r.scroll(), 0);
        r.key(Key::Down);
        assert_eq!(r.scroll(), 1);
        r.key(Key::PageDown);
        assert_eq!(r.scroll(), 10);
        r.key(Key::PageUp);
        assert_eq!(r.scroll(), 1);
        r.key(Key::End);
        assert_eq!(r.scroll(), lines - 10);
        r.key(Key::Down);
        assert_eq!(r.scroll(), lines - 10);
        assert_eq!(r.view().lines.len(), 10);
        r.key(Key::Home);
        assert_eq!(r.scroll(), 0);
        r.wheel(3);
        assert_eq!(r.scroll(), 3);
        r.wheel(-10);
        assert_eq!(r.scroll(), 0);
        r.wheel(isize::MAX);
        assert_eq!(r.scroll(), lines - 10);
        // A taller view keeps the scroll in range.
        r.layout(40, lines + 5, &cols);
        assert_eq!(r.scroll(), 0);
    }

    #[test]
    fn tab_selects_links_wrapping_and_scrolls_them_into_view() {
        let m = long();
        let mut r = reader(&m, "long", 10);
        r.key(Key::NextLink);
        assert_eq!(r.selected(), Some(0));
        let line = r.links()[0].line;
        assert!(r.scroll() <= line && line < r.scroll() + 10);
        assert_eq!(r.view().footer, "Enter: #end");
        r.key(Key::NextLink);
        assert_eq!(r.view().footer, "Enter: Other › #far");
        r.key(Key::NextLink);
        assert_eq!(r.view().footer, "Leaves the manual: https://example.org");
        r.key(Key::NextLink);
        assert_eq!(r.view().footer, "Broken link: no!thing");
        r.key(Key::NextLink);
        assert_eq!(r.selected(), Some(0));
        r.key(Key::PrevLink);
        assert_eq!(r.selected(), Some(3));
        // From the top with nothing selected, Shift+Tab takes the last link.
        let mut r = reader(&m, "long", 10);
        r.key(Key::PrevLink);
        assert_eq!(r.selected(), Some(3));
        assert!(r.scroll() > 0);
    }

    #[test]
    fn follow_opens_pages_anchors_and_never_a_browser() {
        let m = long();
        let mut r = reader(&m, "long", 5);
        // Enter with nothing selected does nothing.
        r.key(Key::Follow);
        assert_eq!(r.page_id(), Some("long"));
        assert!(!r.can_go_back());
        // `#end` scrolls this page to its heading.
        r.key(Key::NextLink);
        let before = r.scroll();
        r.key(Key::Follow);
        assert_eq!(r.page_id(), Some("long"));
        let end = r.laid().unwrap().anchor("end").unwrap();
        assert_eq!(r.scroll(), end.min(r.laid().unwrap().lines.len() - 5));
        assert!(r.scroll() > before);
        r.key(Key::Back);
        assert_eq!(r.scroll(), before);
        // `other#far` opens the page at its heading.
        r.key(Key::NextLink);
        r.key(Key::NextLink);
        assert_eq!(r.selected(), Some(1));
        r.key(Key::Follow);
        r.layout(40, 2, &cols);
        assert_eq!(r.page_id(), Some("other"));
        assert_eq!(r.title(), "Other");
        assert_eq!(top(&r), "Far");
        assert_eq!(r.view().footer, HINTS);
        // An external link stays put and shows its address.
        r.key(Key::Back);
        r.layout(40, 5, &cols);
        assert_eq!(r.page_id(), Some("long"));
        for _ in 0..3 {
            r.key(Key::NextLink);
        }
        r.key(Key::Follow);
        assert_eq!(r.page_id(), Some("long"));
        assert_eq!(r.view().footer, "Open in a browser: https://example.org");
        // A broken link does nothing.
        r.key(Key::NextLink);
        r.key(Key::Follow);
        assert_eq!(r.page_id(), Some("long"));
        // Neither touched the history: Forward still goes to Other.
        assert!(r.can_go_forward());
    }

    #[test]
    fn a_link_to_a_missing_page_does_nothing() {
        let m = manual(&[("a", "[gone](b)")]);
        let mut r = reader(&m, "a", 5);
        r.key(Key::NextLink);
        r.key(Key::Follow);
        assert_eq!(r.page_id(), Some("a"));
        assert!(!r.can_go_back());
    }

    #[test]
    fn back_and_forward_walk_the_history_with_its_scroll() {
        let m = word();
        let mut r = reader(&m, "home", 3);
        r.key(Key::Down);
        r.key(Key::Contents);
        r.layout(40, 3, &cols);
        assert_eq!(r.shown(), &Shown::Contents);
        assert_eq!(r.title(), "Contents");
        assert_eq!(r.scroll(), 0);
        r.key(Key::Back);
        r.layout(40, 3, &cols);
        assert_eq!(r.page_id(), Some("home"));
        assert_eq!(r.scroll(), 1);
        assert!(r.can_go_forward());
        r.key(Key::Forward);
        r.layout(40, 3, &cols);
        assert_eq!(r.shown(), &Shown::Contents);
        assert!(!r.can_go_forward());
        // Back at the start does nothing.
        r.key(Key::Back);
        r.key(Key::Back);
        assert_eq!(r.page_id(), Some("home"));
    }

    #[test]
    fn contents_lists_every_entry_and_follows_to_it() {
        let m = word();
        let mut r = reader(&m, "home", 20);
        r.key(Key::Contents);
        r.layout(40, 20, &cols);
        let lines: Vec<String> = r.view().lines.iter().map(Line::text).collect();
        assert_eq!(lines[0], "Welcome to docxy");
        assert_eq!(lines[1], "Home tab");
        assert!(lines[2].starts_with("  "), "{lines:?}");
        assert_eq!(lines.len(), m.contents.len());
        r.key(Key::NextLink);
        r.key(Key::NextLink);
        r.key(Key::Follow);
        assert_eq!(r.page_id(), Some("home"));
        // Contents on the contents stays, adding no history.
        r.key(Key::Contents);
        r.key(Key::Contents);
        r.key(Key::Back);
        assert_eq!(r.page_id(), Some("home"));
    }

    #[test]
    fn root_and_close() {
        let m = word();
        let mut r = reader(&m, "tables", 5);
        r.key(Key::Root);
        assert_eq!(r.page_id(), Some("welcome"));
        r.key(Key::Back);
        assert_eq!(r.page_id(), Some("tables"));
        assert_eq!(r.key(Key::Close), Outcome::Close);
        assert_eq!(r.key(Key::Down), Outcome::Stay);
    }

    #[test]
    fn keys_before_any_layout_never_overflow_and_end_wins() {
        let m = long();
        let mut r = Reader::open(m, "long", None).unwrap();
        for k in [
            Key::End,
            Key::Down,
            Key::PageDown,
            Key::NextLink,
            Key::PrevLink,
            Key::Down,
        ] {
            assert_eq!(r.key(k), Outcome::Stay, "{k:?}");
        }
        r.wheel(isize::MAX);
        r.layout(40, 10, &cols);
        let lines = r.laid().unwrap().lines.len();
        assert_eq!(r.scroll(), lines - 10, "at the bottom, End wins");
    }

    #[test]
    fn open_at_an_anchor_and_relay_on_resize() {
        let m = long();
        let mut r = Reader::open(m.clone(), "long", Some("end")).unwrap();
        r.layout(40, 3, &cols);
        assert_eq!(top(&r), "End");
        // A narrower view lays the page out again.
        r.layout(4, 3, &cols);
        assert!(r.view().lines.iter().all(|l| cols(&l.text()) <= 4));
    }
}
