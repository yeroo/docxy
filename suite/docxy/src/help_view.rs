//! The Help tab (#1855): an editor's manual ([`help_manual::Bundle`]) read
//! inside the suite. This is its state, without gpui drawing: the page shown,
//! the history, the search box and its hits, link following and the
//! `help-read` JSON. [`lay`] lays a page out; `render.rs` draws it.

use crate::help_manual::{Bundle, Editor};
use ctlcore::json::Json;
use gpui::ScrollHandle;
use helpdocs::md::{self, Block, Link};
use helpdocs::{Hit, Page, Via};
use std::sync::Arc;

pub(crate) mod host;
pub(crate) mod lay;
pub(crate) mod render;

/// A place in the manual: a page, maybe at one of its headings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Loc {
    pub page: String,
    pub anchor: Option<String>,
}

impl Loc {
    pub(crate) fn page(page: &str) -> Loc {
        Loc {
            page: page.to_string(),
            anchor: None,
        }
    }
}

/// What following a link does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Follow {
    /// Open a page (at a heading).
    Navigate(Loc),
    /// Scroll this page to a heading.
    Scroll(String),
    /// Leave the manual: the browser opens the URL.
    External(String),
    /// A link the subset does not know, or one to nothing: drawn as text.
    Nothing,
}

/// Where the body should scroll on the next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollTo {
    Top,
    /// A top-level block (a heading's).
    Block(usize),
}

pub(crate) struct HelpView {
    pub bundle: Arc<Bundle>,
    pub at: Loc,
    /// Places before `at`, the newest last.
    pub back: Vec<Loc>,
    /// Places Back left, the next one last.
    pub forward: Vec<Loc>,
    /// How the last context resolve found its page (`None`: not a resolve).
    pub via: Option<Via>,
    pub query: String,
    pub caret: usize,
    pub sel: bool,
    /// The search box has the keyboard.
    pub search_focused: bool,
    pub hits: Vec<Hit>,
    pub scroll: ScrollHandle,
    /// Taken by the next frame, which scrolls the body there (gpui places
    /// an item once that frame has laid the page out).
    pub scroll_to: std::cell::Cell<Option<ScrollTo>>,
}

impl HelpView {
    /// The manual open at `at` (its root page when `at` names none there).
    pub(crate) fn new(bundle: Arc<Bundle>, at: Loc, via: Option<Via>) -> HelpView {
        let mut v = HelpView {
            bundle,
            at: Loc::page(""),
            back: Vec::new(),
            forward: Vec::new(),
            via,
            query: String::new(),
            caret: 0,
            sel: false,
            search_focused: false,
            hits: Vec::new(),
            scroll: ScrollHandle::new(),
            scroll_to: std::cell::Cell::new(None),
        };
        let at = v.known(at);
        v.show(at);
        v
    }

    pub(crate) fn editor(&self) -> Editor {
        self.bundle.editor
    }

    /// `at`, or the root page when the manual has no such page.
    fn known(&self, at: Loc) -> Loc {
        if self.bundle.manual.page(&at.page).is_some() {
            at
        } else {
            Loc::page(self.root())
        }
    }

    /// The manual's first page.
    pub(crate) fn root(&self) -> &str {
        self.bundle
            .manual
            .pages
            .first()
            .map_or("", |p| p.id.as_str())
    }

    /// The page shown. A bundle always has one (`Bundle` refuses none).
    pub(crate) fn page(&self) -> &Page {
        self.bundle
            .manual
            .page(&self.at.page)
            .or_else(|| self.bundle.manual.pages.first())
            .expect("a help bundle has a page")
    }

    /// The tab's title: `Help: <page title>`.
    pub(crate) fn title(&self) -> String {
        format!("Help: {}", self.page().title)
    }

    /// The top-level block of the heading `slug` on the page shown.
    pub(crate) fn anchor_block(&self, slug: &str) -> Option<usize> {
        anchor_block(&self.page().blocks, slug)
    }

    /// Show `at` with no history step, scrolling to its heading or the top.
    fn show(&mut self, at: Loc) {
        self.at = at;
        self.scroll_to.set(Some(
            self.at
                .anchor
                .as_deref()
                .and_then(|a| self.anchor_block(a))
                .map_or(ScrollTo::Top, ScrollTo::Block),
        ));
    }

    /// Open `at` (the root when the manual has no such page) as a history
    /// step, which drops the forward list. The place shown already is no
    /// step: it only scrolls again. `via` is how a context resolve found it.
    pub(crate) fn navigate(&mut self, at: Loc, via: Option<Via>) {
        let at = self.known(at);
        self.via = via;
        if at != self.at {
            self.back.push(self.at.clone());
            self.forward.clear();
        }
        self.show(at);
    }

    /// Back: the place before. `false` when there is none.
    pub(crate) fn go_back(&mut self) -> bool {
        let Some(at) = self.back.pop() else {
            return false;
        };
        self.forward.push(self.at.clone());
        self.via = None;
        self.show(at);
        true
    }

    /// Forward: the place Back left. `false` when there is none.
    pub(crate) fn go_forward(&mut self) -> bool {
        let Some(at) = self.forward.pop() else {
            return false;
        };
        self.back.push(self.at.clone());
        self.via = None;
        self.show(at);
        true
    }

    /// What following `link` from the page shown does.
    pub(crate) fn follow(&self, link: &Link) -> Follow {
        match link {
            Link::Page { page, anchor } if self.bundle.manual.page(page).is_some() => {
                Follow::Navigate(Loc {
                    page: page.clone(),
                    anchor: anchor.clone(),
                })
            }
            Link::Anchor(slug) if self.anchor_block(slug).is_some() => Follow::Scroll(slug.clone()),
            Link::External(url) => Follow::External(url.clone()),
            _ => Follow::Nothing,
        }
    }

    /// The page's links in page order (the `help-link:<n>` numbering).
    pub(crate) fn links(&self) -> Vec<&Link> {
        md::links(&self.page().blocks)
    }

    /// Follow the page's link `n` inside the manual: navigate or scroll.
    /// Returns what it does, so the host opens an external one.
    pub(crate) fn follow_link(&mut self, n: usize) -> Option<Follow> {
        let link = self.links().get(n).cloned().cloned()?;
        let f = self.follow(&link);
        match &f {
            Follow::Navigate(at) => self.navigate(at.clone(), None),
            Follow::Scroll(slug) => {
                let at = Loc {
                    page: self.at.page.clone(),
                    anchor: Some(slug.clone()),
                };
                self.navigate(at, None);
            }
            Follow::External(_) | Follow::Nothing => {}
        }
        Some(f)
    }

    /// Run the search for the query as it is now: no hits for a blank one.
    pub(crate) fn research(&mut self) {
        self.hits = if self.query.trim().is_empty() {
            Vec::new()
        } else {
            helpdocs::search(&self.bundle.manual, &self.query)
        };
    }

    /// Edit the search box and search again.
    pub(crate) fn edit_query(&mut self, op: crate::field_edit::Op) {
        crate::field_edit::apply(&mut self.query, &mut self.caret, &mut self.sel, op);
        self.research();
    }

    /// Escape in the search box: the contents come back.
    pub(crate) fn clear_query(&mut self) {
        self.query.clear();
        self.caret = 0;
        self.sel = false;
        self.research();
    }

    /// Open search hit `n` (Enter opens the first). `false` when there is
    /// no such hit.
    pub(crate) fn open_hit(&mut self, n: usize) -> bool {
        let Some(page) = self.hits.get(n).map(|h| h.page.clone()) else {
            return false;
        };
        self.navigate(Loc::page(&page), None);
        true
    }

    /// The `help-read` reply; `top_block` is the first top-level block
    /// drawn at the top of the body (clamped to the page's blocks).
    pub(crate) fn read_json(&self, top_block: usize) -> Json {
        let page = self.page();
        let m = &self.bundle.manual;
        let s = |t: &str| Json::Str(t.to_string());
        let n = |v: usize| Json::Num(v as f64);
        let headings = page
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { level, text, slug } => Some(Json::obj(vec![
                    ("level", n(usize::from(*level))),
                    ("text", s(&md::plain(text))),
                    ("slug", s(slug)),
                ])),
                _ => None,
            })
            .collect();
        let links = md::link_texts(&page.blocks)
            .into_iter()
            .enumerate()
            .map(|(i, (link, text))| {
                let (kind, target) = link_kind(link);
                Json::obj(vec![
                    ("index", n(i)),
                    ("text", s(&text)),
                    ("target", s(&target)),
                    ("kind", s(kind)),
                ])
            })
            .collect();
        let images = md::image_alts(&page.blocks)
            .into_iter()
            .map(|(src, alt)| {
                let found = self.bundle.image(src).is_some_and(|b| lay::drawable(b));
                Json::obj(vec![
                    ("src", s(src)),
                    ("alt", s(alt)),
                    ("found", Json::Bool(found)),
                ])
            })
            .collect();
        let contents = m
            .contents
            .iter()
            .map(|e| {
                let title = m.page(&e.id).map_or(e.id.as_str(), |p| p.title.as_str());
                Json::obj(vec![
                    ("id", s(&e.id)),
                    ("depth", n(e.depth)),
                    ("title", s(title)),
                ])
            })
            .collect();
        let hits = self
            .hits
            .iter()
            .map(|h| {
                Json::obj(vec![
                    ("page", s(&h.page)),
                    ("title", s(&h.title)),
                    ("score", n(h.score as usize)),
                ])
            })
            .collect();
        // The body's last child is room below the page, not a block: a top
        // past the blocks is the last one (none on a page without any).
        let top_block = top_block.min(page.blocks.len().saturating_sub(1));
        let top_heading = page
            .blocks
            .iter()
            .take(top_block + 1)
            .rev()
            .find_map(|b| match b {
                Block::Heading { slug, .. } => Some(s(slug)),
                _ => None,
            })
            .unwrap_or(Json::Null);
        Json::obj(vec![
            ("open", Json::Bool(true)),
            ("editor", s(self.editor().name())),
            ("revision", m.revision.as_deref().map_or(Json::Null, s)),
            ("page", s(&page.id)),
            ("title", s(&page.title)),
            ("tab_title", s(&self.title())),
            ("anchor", self.at.anchor.as_deref().map_or(Json::Null, s)),
            (
                "via",
                self.via.map_or(Json::Null, |v| {
                    s(match v {
                        Via::Exact => "exact",
                        Via::Area => "area",
                        Via::Root => "root",
                    })
                }),
            ),
            ("headings", Json::Arr(headings)),
            ("links", Json::Arr(links)),
            ("images", Json::Arr(images)),
            ("contents", Json::Arr(contents)),
            ("query", s(&self.query)),
            ("search_focused", Json::Bool(self.search_focused)),
            ("hits", Json::Arr(hits)),
            ("back", n(self.back.len())),
            ("forward", n(self.forward.len())),
            ("problems", n(m.problems.len())),
            ("top_block", n(top_block)),
            ("top_heading", top_heading),
        ])
    }
}

/// The top-level block of the heading `slug` (headings are only top-level
/// in the subset).
pub(crate) fn anchor_block(blocks: &[Block], slug: &str) -> Option<usize> {
    blocks
        .iter()
        .position(|b| matches!(b, Block::Heading { slug: s, .. } if s == slug))
}

/// A link's kind and target as `help-read` reports them.
pub(crate) fn link_kind(link: &Link) -> (&'static str, String) {
    match link {
        Link::Page {
            page,
            anchor: Some(a),
        } => ("page", format!("{page}#{a}")),
        Link::Page { page, anchor: None } => ("page", page.clone()),
        Link::Anchor(a) => ("anchor", format!("#{a}")),
        Link::External(u) => ("external", u.clone()),
        Link::Other(t) => ("other", t.clone()),
    }
}

#[cfg(test)]
mod tests;
