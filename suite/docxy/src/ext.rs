//! Extension surfaces: a build of the suite may link a crate that draws a
//! tab kind the public suite leaves as a placeholder (today: Mail). The
//! public `suite` binary passes [`Extensions::default`], so nothing here
//! changes it; the seam names no particular extension.
//!
//! An extension's surface is a gpui view the tab hosts as its body
//! (`Surface::Ext`). The tab owns it like any other surface, so View >
//! Window > New Window and tearing the tab off the strip carry the same view,
//! state and all, into the new window. The suite still draws the title bar,
//! the tab strip and the status line; the view draws everything below the
//! tab strip (its own ribbon included) and takes the keys while its tab is
//! the active one.

use gpui::{AnyView, App, FocusHandle, Global, actions};
use std::rc::Rc;

/// A read-only view of a document an extension's view may embed (#1873):
/// the suite's own document renderer in web layout, with selection, copy,
/// zoom, scrolling and hyperlinks handed to the extension. See
/// [`DocView::new`].
pub use crate::doc_view::{DocView, LinkClick};

actions!(
    docxy_ext,
    [
        /// Opens the suite's File backstage (New, Open, Recent, Settings).
        /// An extension's own File command dispatches it from its view.
        ShowBackstage
    ]
);

/// A tab body an extension draws.
pub trait TabSurface: 'static {
    /// The view drawn as the tab's body.
    fn view(&self) -> AnyView;
    /// What the suite focuses when the tab becomes active, so the view's
    /// own key handling sees the keys first.
    fn focus_handle(&self, cx: &App) -> FocusHandle;
    /// The tab's title (the folder shown, say). `None` keeps the suite's.
    fn title(&self, _cx: &App) -> Option<String> {
        None
    }
    /// The suite's theme turned light or dark (View > Theme, Auto following
    /// the OS). The view may follow it.
    fn set_dark(&self, _dark: bool, _cx: &mut App) {}
}

/// Makes the surface of a new tab of one kind. An error is said on the tab's
/// status line and the tab keeps the placeholder.
pub type Factory = Rc<dyn Fn(&mut App) -> Result<Box<dyn TabSurface>, String>>;

/// What a build links in. Every field is optional.
#[derive(Default)]
pub struct Extensions {
    /// Draws Mail tabs (File > New > Mail, a restored Mail tab).
    pub mail: Option<Factory>,
}

struct Installed(Extensions);

impl Global for Installed {}

pub(crate) fn install(cx: &mut App, extensions: Extensions) {
    cx.set_global(Installed(extensions));
}

/// A new Mail surface, when an extension draws Mail: `None` without one.
pub(crate) fn new_mail(cx: &mut App) -> Option<Result<Box<dyn TabSurface>, String>> {
    let factory = cx.try_global::<Installed>()?.0.mail.clone()?;
    Some(factory(cx))
}
