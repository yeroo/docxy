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
use docxy_suite::ext::{
    self, CaretFormat, DocFormat, DocLink, DocView, Extensions, LinkClick, TabSurface,
};
use gpui::{
    AnyView, App, AppContext as _, Edges, Entity, FocusHandle, Hsla, Pixels, Window, px, rgb,
};
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
        // The host's page inset and dark page colour (#2003), and back to
        // the defaults.
        v.set_page_inset(
            Some(Edges {
                top: px(12.),
                right: px(22.),
                bottom: px(12.),
                left: px(22.),
            }),
            cx,
        );
        let _inset: Option<Edges<Pixels>> = v.page_inset();
        v.set_page_background(Some(rgb(0x262626).into()), cx);
        let _bg: Option<Hsla> = v.page_background();
        v.set_page_inset(None, cx);
        v.set_page_background(None, cx);
        // Print Layout (#1888): page sheets, the page count in either
        // layout, and back to the flow.
        v.set_layout(ext::DocLayout::Print, cx);
        let _pages: usize = v.page_count();
        let _sheets = match v.layout() {
            ext::DocLayout::Print => true,
            // Non-exhaustive: a later layout is another arm.
            _ => false,
        };
        v.set_layout(ext::DocLayout::default(), cx);
    });
    Reader { view, followed }
}

/// A mail body: the view made editable, the host ribbon's commands, its
/// pressed states, its Insert, Copy and Cut (#1977), its link dialog
/// (#1952), the change event and what is exported (#1947).
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
            // The host's link dialog (#1952).
            DocFormat::InsertLink {
                text: "docs".into(),
                url: "https://example.com/".into(),
            },
            DocFormat::EditLink {
                text: String::new(),
                url: "#top".into(),
            },
            DocFormat::RemoveLink,
        ] {
            let _ran: bool = v.format(f, cx);
        }
        let shown: CaretFormat = v.caret_format();
        let _pressed: (bool, bool, bool, bool) =
            (shown.bold, shown.italic, shown.underline, shown.strike);
        let _boxes: (Option<String>, Option<f32>) = (shown.font, shown.size);
        let _colours: (Option<String>, Option<String>) = (shown.color, shown.highlight);
        let _lists: (bool, bool, Align) = (shown.bullets, shown.numbering, shown.align);
        // The link at the caret prefills the host's Edit Link dialog.
        let link: Option<DocLink> = shown.link;
        let _dialog: Option<(String, String)> = link.map(|l| (l.text, l.url));
        let _undone: bool = v.can_undo() && v.undo(cx);
        let _redone: bool = v.can_redo() && v.redo(cx);
        // The host's Insert, Copy and Cut buttons (#1977).
        let _inserted: bool = v.insert_text("Kind regards,\nA. Sender", cx);
        let _copied: bool = v.can_copy() && v.copy(cx);
        let _cut: bool = v.can_copy() && v.editable() && v.cut(cx);
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

/// A surface that describes its ribbon to the suite and reads the suite's
/// theme (#1870), every control kind built through the public constructors
/// only, as an extension crate must (the types are `#[non_exhaustive]`).
struct Mailbox {
    reader: Reader,
    flagged: Rc<Cell<bool>>,
    theme: Rc<Cell<Option<ext::SuiteTheme>>>,
}

impl TabSurface for Mailbox {
    fn view(&self) -> AnyView {
        self.reader.view()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.reader.focus_handle(cx)
    }

    fn ribbon(&self, _cx: &App) -> Option<ext::Ribbon> {
        use ext::{
            Ribbon, RibbonCommand, RibbonControl, RibbonDropdown, RibbonGallery, RibbonGroup,
            RibbonIcon, RibbonSplit, RibbonTab,
        };
        let icon = RibbonIcon::svg(b"<svg xmlns='http://www.w3.org/2000/svg'/>".as_slice());
        let _source: &[u8] = icon.source();
        let reply = RibbonCommand::new("reply", "Reply")
            .icon(icon)
            .tip("Reply to the sender")
            .key_tip("R");
        let respond = RibbonGroup::new(
            "Respond",
            vec![
                RibbonControl::Large(reply),
                RibbonControl::Small(vec![
                    RibbonCommand::new("forward", "Forward"),
                    RibbonCommand::new("junk", "Junk").enabled(false),
                ]),
                RibbonControl::Toggle(
                    RibbonCommand::new("flag", "Flag").checked(self.flagged.get()),
                ),
                RibbonControl::Split(RibbonSplit::new(
                    RibbonCommand::new("move", "Move"),
                    vec![RibbonCommand::new("move.inbox", "Inbox")],
                )),
                RibbonControl::Dropdown(RibbonDropdown::new(
                    RibbonCommand::new("sort", "Sort"),
                    vec![RibbonCommand::new("sort.date", "By Date")],
                )),
                RibbonControl::Gallery(
                    RibbonGallery::new("density", vec![RibbonCommand::new("d1", "Compact")])
                        .tip("Density")
                        .key_tip("D"),
                ),
            ],
        )
        .priority(120);
        let mut ribbon = Ribbon::new(vec![RibbonTab::new("Message", "M", vec![respond])]);
        // Its fields stay readable and writable.
        ribbon.tabs[0].groups[0].title.push_str(" now");
        let _names: Vec<&str> = ribbon.tabs.iter().map(|t| t.name.as_str()).collect();
        Some(ribbon)
    }

    fn ribbon_action(&self, id: &str, _window: &mut Window, cx: &mut App) {
        if id == "flag" {
            self.flagged.set(!self.flagged.get());
        }
        // The window-free call a model or a background task makes too.
        ext::ribbon_changed(cx);
    }

    fn theme_changed(&self, theme: &ext::SuiteTheme, cx: &mut App) {
        let now: ext::SuiteTheme = ext::suite_theme(cx);
        let _colours: [Hsla; 7] = [
            now.background,
            now.panel,
            now.border,
            now.accent,
            now.text,
            now.muted_text,
            now.selection,
        ];
        let _dark: bool = theme.dark;
        self.theme.set(Some(*theme));
    }
}

#[test]
fn an_extension_ribbon_is_public_api() {
    let _ = <Mailbox as TabSurface>::ribbon as fn(&Mailbox, &App) -> Option<ext::Ribbon>;
    let _ = ext::suite_theme as fn(&App) -> ext::SuiteTheme;
    let _ = ext::ribbon_changed as fn(&mut App);
    // A command is enabled and not checked unless set; a group's priority
    // is the middle of the suite's.
    let c = ext::RibbonCommand::new("id", "Label");
    assert!(c.enabled && !c.checked && c.icon.is_none());
    assert!(c.tip.is_empty() && c.key_tip.is_empty());
    let g = ext::RibbonGroup::new("Group", vec![ext::RibbonControl::Large(c)]);
    assert_eq!(g.priority, 50);
    assert_eq!(g.priority(7).priority, 7);
    assert!(ext::Ribbon::default().tabs.is_empty());
}

/// A surface that adds pages to the suite's File backstage and closes its
/// own tab (#1923): its pages are made once and handed out as clones.
struct Account {
    reader: Reader,
    pages: Vec<ext::BackstagePage>,
}

impl TabSurface for Account {
    fn view(&self) -> AnyView {
        self.reader.view()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.reader.focus_handle(cx)
    }

    fn backstage_pages(&self, _cx: &App) -> Vec<ext::BackstagePage> {
        self.pages.clone()
    }

    fn ribbon_action(&self, id: &str, window: &mut Window, cx: &mut App) {
        // An Exit command closes the tab.
        if id == "exit" {
            window.dispatch_action(Box::new(ext::CloseTab), cx);
        }
    }
}

/// The pages: an account page at the top with an icon, options at the
/// bottom; both views are the reader's document view here.
fn account_pages(view: Entity<DocView>) -> Vec<ext::BackstagePage> {
    use ext::{BackstageGroup, BackstagePage, RibbonIcon};
    let icon = RibbonIcon::svg(b"<svg xmlns='http://www.w3.org/2000/svg'/>".as_slice());
    let info = BackstagePage::new("info", "Account Information", view.clone()).icon(icon);
    let options = BackstagePage::new("options", "Options", view).group(BackstageGroup::Bottom);
    // Its fields stay readable.
    let _ids: [&str; 2] = [info.id.as_str(), options.id.as_str()];
    let _view: AnyView = info.view.clone();
    vec![info, options]
}

#[test]
fn extension_backstage_pages_are_public_api() {
    let _ =
        <Account as TabSurface>::backstage_pages as fn(&Account, &App) -> Vec<ext::BackstagePage>;
    let _ = account_pages as fn(Entity<DocView>) -> Vec<ext::BackstagePage>;
    let _: Box<dyn gpui::Action> = Box::new(ext::CloseTab);
    // A page is a top one unless set.
    assert_eq!(ext::BackstageGroup::default(), ext::BackstageGroup::Top);
}
