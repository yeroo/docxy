//! An extension crate can embed the suite's document view through the
//! public API alone (#1873), read-only or editable (#1947). This file is a crate of its own against
//! `docxy_suite`, so a `pub(crate)` slip in what an extension needs fails to
//! compile here. The surface below is type-checked, never run: building
//! gpui entities needs a window the test run has no display for (the
//! harness's `ext-doc-view.uit` drives the real thing).

#![allow(dead_code)]

use docxcore::model::Align;
use docxcore::model::Document;
use docxcore::package::Package;
use docxy_suite::ext::{self, CaretFormat, DocFormat, DocView, Extensions, LinkClick, TabSurface};
use gpui::{AnyView, App, AppContext as _, Entity, FocusHandle, Window};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// An extension's tab body: a document view and the links it was handed.
struct Reader {
    view: Entity<DocView>,
    followed: Rc<RefCell<Vec<String>>>,
}

impl TabSurface for Reader {
    fn view(&self) -> AnyView {
        self.view.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.view.read(cx).focus_handle()
    }

    fn set_dark(&self, dark: bool, cx: &mut App) {
        // Follow the theme into the content too.
        self.view.update(cx, |v, cx| v.set_content_dark(dark, cx));
    }
}

/// Every public call an extension makes on the view.
fn embed(doc: Document, pkg: Package, cx: &mut App) -> Reader {
    let followed: Rc<RefCell<Vec<String>>> = Rc::default();
    let view = cx.new(|cx| DocView::new(doc, Some(pkg), cx));
    view.update(cx, |v, cx| {
        let followed = followed.clone();
        v.on_link(move |url: &str, _window: &mut Window, _cx: &mut App| {
            followed.borrow_mut().push(url.to_string())
        });
        v.set_link_click(LinkClick::Click, cx);
        v.set_link_click(LinkClick::CtrlClick, cx);
        v.set_content_dark(false, cx);
        let z: f32 = v.zoom();
        v.set_zoom(z + 0.5, cx);
        let _selected: String = v.selected_text();
        let _focus: FocusHandle = v.focus_handle();
        v.set_document(Document { body: vec![] }, None, cx);
    });
    Reader { view, followed }
}

/// A mail body: the view made editable, the host ribbon's commands, its
/// pressed states, the change event and what is exported (#1947).
fn compose(doc: Document, pkg: Package, cx: &mut App) -> Rc<Cell<usize>> {
    let changes: Rc<Cell<usize>> = Rc::default();
    let view = cx.new(|cx| DocView::new(doc, Some(pkg), cx));
    view.update(cx, |v, cx| {
        let changes = changes.clone();
        v.on_change(move |_cx: &mut App| changes.set(changes.get() + 1));
        v.set_editable(true, cx);
        let _on: bool = v.editable();
        for f in [
            DocFormat::Bold,
            DocFormat::Italic,
            DocFormat::Underline,
            DocFormat::Font("Georgia".into()),
            DocFormat::Size(12.),
            DocFormat::Color(Some("1F7A3D".into())),
            DocFormat::Color(None),
            DocFormat::Highlight(Some("yellow".into())),
            DocFormat::Highlight(None),
            DocFormat::Bullets,
            DocFormat::Numbering,
            DocFormat::Indent,
            DocFormat::Outdent,
            DocFormat::Align(Align::Center),
        ] {
            let _ran: bool = v.format(f, cx);
        }
        let shown: CaretFormat = v.caret_format();
        let _pressed: (bool, bool, bool, bool) =
            (shown.bold, shown.italic, shown.underline, shown.strike);
        let _boxes: (Option<String>, Option<f32>) = (shown.font, shown.size);
        let _colours: (Option<String>, Option<String>) = (shown.color, shown.highlight);
        let _lists: (bool, bool, Align) = (shown.bullets, shown.numbering, shown.align);
        let _undone: bool = v.can_undo() && v.undo(cx);
        let _redone: bool = v.can_redo() && v.redo(cx);
        let _body: Document = v.document();
        let _pkg: Option<Package> = v.package();
    });
    changes
}

/// A build that links the extension: Mail tabs are its reader.
fn extensions() -> Extensions {
    let factory: ext::Factory = Rc::new(|cx: &mut App| {
        let pkg = docxcore::package::new_package(Document { body: vec![] });
        Ok(Box::new(embed(Document { body: vec![] }, pkg, cx)) as Box<dyn TabSurface>)
    });
    Extensions {
        mail: Some(factory),
    }
}

#[test]
fn the_document_view_is_public_api() {
    // Naming the functions is the test: they compiled against the public
    // API. The default link mode is Word's.
    let _ = extensions as fn() -> Extensions;
    let _ = compose as fn(Document, Package, &mut App) -> Rc<Cell<usize>>;
    assert_eq!(LinkClick::default(), LinkClick::CtrlClick);
}
