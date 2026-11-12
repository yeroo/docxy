//! The harness's test extension (#1873): a Mail surface whose body is a
//! header line over an [`ext::DocView`] of a fixture document, so a `.uit`
//! case can drive the embedded document view as an extension would place it.
//! Installed only in a harness instance whose script asks for it
//! (`ext fixture`, [`crate::harness::ext_fixture_requested`]); the public
//! `suite` binary and any other run never make it.
//!
//! It uses only what an extension crate can: the public items of
//! [`crate::ext`] (`tests/ext_doc_view_api.rs` holds the same surface to that
//! from outside the crate), records every link the view hands it and counts
//! its change events (#1947).
//!
//! `ext fixture ribbon` (#1870) has it describe a two-tab ribbon to the suite
//! ([`fixture_ribbon`]): it records each command the suite runs, flips its
//! Flag toggle and its density gallery on their commands, and keeps the
//! last theme the suite told it ([`RibbonState`]). Its View tab's Exit
//! closes the tab through [`ext::CloseTab`] (#1923).
//!
//! Every variant adds two File pages (#1923, [`fixture_pages`]): Fixture
//! Account (top, with an icon) and Fixture Options (bottom), each a
//! [`FixturePage`] counting its renders and recording where it and its Close
//! tab button painted; the button dispatches [`ext::CloseTab`].

use crate::ext::{
    self, BackstageGroup, BackstagePage, DocView, Ribbon, RibbonCommand, RibbonControl,
    RibbonDropdown, RibbonGallery, RibbonGroup, RibbonIcon, RibbonSplit, RibbonTab, SuiteTheme,
    TabSurface,
};
use docxcore::model::Document;
use docxcore::package::{Package, new_package};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// The target of the fixture document's hyperlink.
pub(crate) const LINK_URL: &str = "https://example.com/docxy";
/// The target of its indented paragraph's link.
pub(crate) const INDENTED_URL: &str = "https://example.com/indented";
/// The link in the fitted (100%) table's left cell (#1895).
pub(crate) const CELL_URL: &str = "https://example.com/cell";

/// The fitted table (#1895), the `newsletter-tables.html` shape: a 100%
/// table of two 50% cells whose text wraps when the view is narrow, through
/// the HTML import as a mail body comes.
fn fitted_html() -> String {
    format!(
        "<table width=\"100%\" border=\"1\"><tr>\
         <td width=\"50%\">Fitted left column, wrapping when narrow, with the \
         <a href=\"{CELL_URL}\">cell link</a> inside it.</td>\
         <td width=\"50%\">Fitted right column, also long enough to wrap in a \
         narrow view.</td></tr></table>"
    )
}

/// The fixture's view: what the harness downcasts the Mail tab's body to.
pub(crate) struct FixtureView {
    pub(crate) doc: Entity<DocView>,
    /// Each link the view handed the extension, in order.
    pub(crate) link_calls: Rc<RefCell<Vec<String>>>,
    /// How many change events the view sent (#1947).
    pub(crate) change_calls: Rc<Cell<usize>>,
    /// Each link-under-the-pointer report, in order: `None` off every link
    /// (#1916).
    pub(crate) hover_calls: Rc<RefCell<Vec<Option<ext::LinkHover>>>>,
    /// Each followed link with its text, from `on_link_with_text` (#1916).
    pub(crate) link_texts: Rc<RefCell<Vec<ext::LinkHover>>>,
    /// The header line's own focus, as a host's chrome beside the view may
    /// have (a press on it takes the keys from the view, #2003).
    header_focus: FocusHandle,
    /// The ribbon it describes to the suite, in `ext fixture ribbon` (#1870).
    pub(crate) ribbon: Option<Rc<RibbonState>>,
    /// Every theme the suite told it, the last one last (#1870).
    pub(crate) themes: Rc<RefCell<Vec<SuiteTheme>>>,
}

impl FixtureView {
    /// Whether its header line has the keys (#1870).
    pub(crate) fn header_focused(&self, window: &Window) -> bool {
        self.header_focus.is_focused(window)
    }

    /// Its ribbon state and the themes it was told, for the harness.
    pub(crate) fn clone_parts(&self) -> (Option<Rc<RibbonState>>, Vec<SuiteTheme>) {
        (self.ribbon.clone(), self.themes.borrow().clone())
    }
}

/// The ribbon variant's state (#1870).
#[derive(Default)]
pub(crate) struct RibbonState {
    /// Each command id the suite ran, in order.
    pub(crate) actions: RefCell<Vec<String>>,
    /// The Flag toggle, flipped by its own command.
    pub(crate) flag: Cell<bool>,
    /// Whether Archive is enabled (`ext-ribbon-set`).
    pub(crate) archive_enabled: Cell<bool>,
    /// The density gallery's checked item.
    pub(crate) density: Cell<usize>,
}

/// The density gallery's items: id and label.
const DENSITIES: [(&str, &str); 3] = [
    ("fx.density.compact", "Compact"),
    ("fx.density.cozy", "Cozy"),
    ("fx.density.roomy", "Roomy"),
];

/// The Reply command's icon: an arrow, in the shape of Fluent's.
const REPLY_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 20 20\" fill=\"currentColor\"><path d=\"M8.35 4.15a.5.5 0 0 1 0 .7L4.71 8.5H11a6 6 0 0 1 6 6v1a.5.5 0 0 1-1 0v-1a5 5 0 0 0-5-5H4.7l3.65 3.65a.5.5 0 0 1-.7.7l-4.5-4.5a.5.5 0 0 1 0-.7l4.5-4.5a.5.5 0 0 1 .7 0Z\"/></svg>";

/// A tag's icon: a filled circle.
const TAG_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 20 20\" fill=\"currentColor\"><circle cx=\"10\" cy=\"10\" r=\"6\"/></svg>";

/// The ribbon the variant describes: Message (KeyTip `M`) with Respond
/// (Reply, a column of three, the Flag toggle), Move (the Move split, a
/// disabled-until-set Archive) and Tags (the lowest priority: a column of
/// three tags and two large buttons with no icon, wide enough to collapse
/// in a narrow window), and View (`V`) with a Sort drop-down and the
/// density gallery.
pub(crate) fn fixture_ribbon(state: &RibbonState) -> Ribbon {
    let c = RibbonCommand::new;
    Ribbon::new(vec![
        RibbonTab::new(
            "Message",
            "M",
            vec![
                RibbonGroup::new(
                    "Respond",
                    vec![
                        RibbonControl::Large(
                            c("fx.reply", "Reply")
                                .icon(RibbonIcon::svg(REPLY_SVG.as_bytes()))
                                .tip("Reply to the sender")
                                .key_tip("R"),
                        ),
                        RibbonControl::Small(vec![
                            c("fx.reply-all", "Reply All")
                                .icon(RibbonIcon::svg(REPLY_SVG.as_bytes()))
                                .key_tip("A"),
                            c("fx.forward", "Forward").key_tip("W"),
                            c("fx.meeting", "Meeting"),
                        ]),
                        RibbonControl::Toggle(
                            c("fx.flag", "Flag").key_tip("G").checked(state.flag.get()),
                        ),
                    ],
                )
                .priority(100),
                RibbonGroup::new(
                    "Move",
                    vec![
                        RibbonControl::Split(RibbonSplit::new(
                            c("fx.move", "Move").key_tip("MV"),
                            vec![
                                c("fx.move.inbox", "Inbox").key_tip("I"),
                                c("fx.move.later", "Later").key_tip("L"),
                            ],
                        )),
                        RibbonControl::Large(
                            c("fx.archive", "Archive")
                                .key_tip("H")
                                .enabled(state.archive_enabled.get()),
                        ),
                    ],
                )
                .priority(40),
                RibbonGroup::new(
                    "Tags",
                    vec![
                        RibbonControl::Small(
                            [
                                ("fx.tag.red", "Red tag"),
                                ("fx.tag.blue", "Blue tag"),
                                ("fx.tag.green", "Green tag"),
                            ]
                            .into_iter()
                            .map(|(id, label)| {
                                c(id, label).icon(RibbonIcon::svg(TAG_SVG.as_bytes()))
                            })
                            .collect(),
                        ),
                        RibbonControl::Large(c("fx.categorize", "Assign Category")),
                        RibbonControl::Large(c("fx.follow-up", "Follow Up Later")),
                    ],
                )
                .priority(20),
            ],
        ),
        RibbonTab::new(
            "View",
            "V",
            vec![
                RibbonGroup::new(
                    "Arrange",
                    vec![
                        RibbonControl::Dropdown(RibbonDropdown::new(
                            c("fx.sort", "Sort").key_tip("S"),
                            vec![
                                c("fx.sort.date", "By Date").key_tip("D"),
                                c("fx.sort.from", "By Sender").key_tip("N"),
                            ],
                        )),
                        RibbonControl::Gallery(
                            RibbonGallery::new(
                                "fx.density",
                                DENSITIES
                                    .iter()
                                    .enumerate()
                                    .map(|(i, (id, label))| {
                                        c(id, label).checked(i == state.density.get())
                                    })
                                    .collect(),
                            )
                            .tip("Density")
                            .key_tip("Y"),
                        ),
                    ],
                ),
                // After Arrange, so what the case asserts of Arrange stays.
                RibbonGroup::new(
                    "Window",
                    vec![RibbonControl::Large(c(EXIT_ID, "Exit").key_tip("X"))],
                ),
            ],
        ),
    ])
}

/// The View tab's Exit (#1923): closes the tab through [`ext::CloseTab`].
pub(crate) const EXIT_ID: &str = "fx.exit";

/// What a command of the variant's ribbon does to its state.
fn run_ribbon_action(state: &RibbonState, id: &str) {
    state.actions.borrow_mut().push(id.to_string());
    if id == "fx.flag" {
        state.flag.set(!state.flag.get());
    }
    if let Some(i) = DENSITIES.iter().position(|(d, _)| *d == id) {
        state.density.set(i);
    }
}

impl Render for FixtureView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let calls = self.link_calls.borrow().len();
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .id("ext-fixture-header")
                    .track_focus(&self.header_focus)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, window, cx| {
                            this.header_focus.focus(window, cx);
                        }),
                    )
                    .px_2()
                    .py_1()
                    .when(calls > 0, |d| d.font_weight(FontWeight::BOLD))
                    .child(format!("Fixture extension ({calls} links followed)")),
            )
            .child(div().flex_1().min_h(px(0.)).child(self.doc.clone()))
    }
}

struct FixtureMail {
    root: Entity<FixtureView>,
    /// Its File pages (#1923), made once: each read returns the same views.
    pages: Vec<BackstagePage>,
}

/// The top page's id and label, and the bottom page's (#1923).
pub(crate) const ACCOUNT_PAGE: (&str, &str) = ("fx.account", "Fixture Account");
pub(crate) const OPTIONS_PAGE: (&str, &str) = ("fx.options", "Fixture Options");

/// A File page of the fixture's (#1923): its title, a Close tab button, how
/// many times it rendered and where it and the button last painted (the
/// harness's `ext-page-read` and `ext-page-close:<id>` region).
pub(crate) struct FixturePage {
    title: &'static str,
    pub(crate) renders: Rc<Cell<usize>>,
    pub(crate) bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(crate) close_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

/// A canvas recording its bounds at paint into `cell`, filling its parent.
fn record_bounds(cell: &Rc<Cell<Option<Bounds<Pixels>>>>) -> impl IntoElement {
    let cell = cell.clone();
    canvas(
        move |b: Bounds<Pixels>, _: &mut Window, _: &mut App| cell.set(Some(b)),
        |_, _, _, _| {},
    )
    .absolute()
    .left_0()
    .top_0()
    .right_0()
    .bottom_0()
}

impl Render for FixturePage {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div()
            .relative()
            .size_full()
            .p_8()
            .flex()
            .flex_col()
            .gap_4()
            .child(record_bounds(&self.bounds))
            .child(div().text_size(px(20.)).child(self.title))
            .child(
                div()
                    .id("ext-page-close")
                    .relative()
                    .px_3()
                    .py_1()
                    .border_1()
                    .border_color(rgb(0x808080))
                    .rounded_sm()
                    .cursor_pointer()
                    .child(record_bounds(&self.close_bounds))
                    .child("Close tab")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(ext::CloseTab), cx);
                    }),
            )
    }
}

/// The fixture's File pages (#1923): Fixture Account at the top (with the
/// Reply icon), Fixture Options at the bottom.
fn fixture_pages(cx: &mut App) -> Vec<BackstagePage> {
    let mut page = |(id, title): (&'static str, &'static str)| {
        let view = cx.new(|_| FixturePage {
            title,
            renders: Rc::default(),
            bounds: Rc::default(),
            close_bounds: Rc::default(),
        });
        BackstagePage::new(id, title, view)
    };
    vec![
        page(ACCOUNT_PAGE).icon(RibbonIcon::svg(REPLY_SVG.as_bytes())),
        page(OPTIONS_PAGE).group(BackstageGroup::Bottom),
    ]
}

impl FixtureMail {
    fn ribbon_state(&self, cx: &App) -> Option<Rc<RibbonState>> {
        self.root.read(cx).ribbon.clone()
    }
}

impl TabSurface for FixtureMail {
    fn view(&self) -> AnyView {
        self.root.clone().into()
    }

    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.root.read(cx).doc.read(cx).focus_handle()
    }

    fn ribbon(&self, cx: &App) -> Option<Ribbon> {
        self.ribbon_state(cx).map(|s| fixture_ribbon(&s))
    }

    fn ribbon_action(&self, id: &str, window: &mut Window, cx: &mut App) {
        if let Some(s) = self.ribbon_state(cx) {
            run_ribbon_action(&s, id);
        }
        // Exit closes the tab, as an extension's own Exit would (#1923).
        if id == EXIT_ID {
            window.dispatch_action(Box::new(ext::CloseTab), cx);
        }
    }

    fn theme_changed(&self, theme: &SuiteTheme, cx: &mut App) {
        self.root.read(cx).themes.borrow_mut().push(*theme);
    }

    fn backstage_pages(&self, _cx: &App) -> Vec<BackstagePage> {
        self.pages.clone()
    }
}

/// The fixture's Mail factory; with `ribbon` its surface describes
/// [`fixture_ribbon`] to the suite (#1870).
pub(crate) fn factory(ribbon: bool) -> ext::Factory {
    Rc::new(move |cx: &mut App| {
        let (doc, pkg) = fixture_document();
        let calls: Rc<RefCell<Vec<String>>> = Rc::default();
        let changes: Rc<Cell<usize>> = Rc::default();
        let hovers: Rc<RefCell<Vec<Option<ext::LinkHover>>>> = Rc::default();
        let texts: Rc<RefCell<Vec<ext::LinkHover>>> = Rc::default();
        let view = cx.new(|cx| DocView::new(doc, Some(pkg), cx));
        view.update(cx, |v, _| {
            let calls = calls.clone();
            v.on_link(move |url, _window, _cx| calls.borrow_mut().push(url.to_string()));
            let changes = changes.clone();
            v.on_change(move |_cx| changes.set(changes.get() + 1));
            let hovers = hovers.clone();
            v.on_link_hover(move |link, _window, _cx| hovers.borrow_mut().push(link.cloned()));
            let texts = texts.clone();
            v.on_link_with_text(move |link, _window, _cx| texts.borrow_mut().push(link.clone()));
        });
        let root = cx.new(|cx| FixtureView {
            doc: view,
            link_calls: calls,
            change_calls: changes,
            hover_calls: hovers,
            link_texts: texts,
            header_focus: cx.focus_handle(),
            ribbon: ribbon.then(Rc::default),
            themes: Rc::default(),
        });
        let pages = fixture_pages(cx);
        Ok(Box::new(FixtureMail { root, pages }) as Box<dyn TabSurface>)
    })
}

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

fn para(inner: &str) -> String {
    format!("<w:p>{inner}</w:p>")
}

fn run(text: &str) -> String {
    format!("<w:r><w:t xml:space=\"preserve\">{text}</w:t></w:r>")
}

fn cell(text: &str) -> String {
    cell_w(text, 2400)
}

fn cell_w(text: &str, w: u32) -> String {
    format!(
        "<w:tc><w:tcPr><w:tcW w:w=\"{w}\" w:type=\"dxa\"/></w:tcPr>{}</w:tc>",
        para(&run(text))
    )
}

/// The fixture document and its package, built here (no binary fixture): a
/// heading, `Alpha Beta Gamma` (Beta bold), a hyperlink `docxy link` to
/// [`LINK_URL`], an indented paragraph that starts with a link
/// (`Indented link`, to [`INDENTED_URL`]), an inline picture
/// (`uiharness/fixtures/logo.png`), a 2x2 table `A1 B1 / A2 B2`, the
/// fitted 100% table ([`fitted_html`], imported, with a link to
/// [`CELL_URL`]; only in the model, not the package's part), a fixed
/// (`dxa`) table of two 6000-twip cells `Fixed left` / `Right Omega Zeta`
/// (#1895), then `Line 1`..`Line 80` so it scrolls.
pub(crate) fn fixture_document() -> (Document, Package) {
    let mut pkg = new_package(Document { body: vec![] });
    let png: &[u8] = include_bytes!("../../../uiharness/fixtures/logo.png");
    let kind = docxcore::image::sniff(png).expect("logo.png is a png");
    let rid = pkg
        .add_part_image("word/document.xml", png, &kind)
        .expect("the picture is added");
    let rels_name = "word/_rels/document.xml.rels";
    let rels = pkg.part_text(rels_name).expect("document rels").replace(
        "</Relationships>",
        &format!(
            "<Relationship Id=\"rIdLink1\" Type=\"{R}/hyperlink\" Target=\"{LINK_URL}\" TargetMode=\"External\"/>\
             <Relationship Id=\"rIdLink2\" Type=\"{R}/hyperlink\" Target=\"{INDENTED_URL}\" TargetMode=\"External\"/></Relationships>"
        ),
    );
    assert!(pkg.set_part(rels_name, rels.into_bytes()));
    let styles = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:styles xmlns:w=\"{W}\"><w:style w:type=\"paragraph\" w:styleId=\"Heading1\">\
         <w:name w:val=\"heading 1\"/><w:rPr><w:b/><w:sz w:val=\"32\"/></w:rPr></w:style></w:styles>"
    );
    assert!(pkg.set_part("word/styles.xml", styles.into_bytes()));
    let mut body = String::new();
    body += &para(&format!(
        "<w:pPr><w:pStyle w:val=\"Heading1\"/></w:pPr>{}",
        run("Embedded view")
    ));
    body += &para(&format!(
        "{}<w:r><w:rPr><w:b/></w:rPr><w:t>Beta</w:t></w:r>{}",
        run("Alpha "),
        run(" Gamma")
    ));
    body += &para(&format!(
        "{}<w:hyperlink r:id=\"rIdLink1\">{}</w:hyperlink>{}",
        run("Follow the "),
        run("docxy link"),
        run(" here.")
    ));
    // A row that starts with a link, indented: a press in its indent is
    // beside the row, never on the link (#1873 r1).
    body += &para(&format!(
        "<w:pPr><w:ind w:left=\"1440\"/></w:pPr><w:hyperlink r:id=\"rIdLink2\">{}</w:hyperlink>{}",
        run("Indented link"),
        run(" after it")
    ));
    body += &para(&docxcore::image::inline_picture_run(
        &rid, 609600, 304800, 1, "logo.png",
    ));
    body += &format!(
        "<w:tbl><w:tblPr><w:tblW w:w=\"4800\" w:type=\"dxa\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"2400\"/><w:gridCol w:w=\"2400\"/></w:tblGrid>\
         <w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
        cell("A1"),
        cell("B1"),
        cell("A2"),
        cell("B2")
    );
    body += &format!(
        "<w:tbl><w:tblPr><w:tblW w:w=\"12000\" w:type=\"dxa\"/></w:tblPr>\
         <w:tblGrid><w:gridCol w:w=\"6000\"/><w:gridCol w:w=\"6000\"/></w:tblGrid>\
         <w:tr>{}{}</w:tr></w:tbl>",
        cell_w("Fixed left", 6000),
        cell_w("Right Omega Zeta", 6000)
    );
    for i in 1..=80 {
        body += &para(&run(&format!("Line {i}")));
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:body>{body}</w:body></w:document>"
    );
    assert!(pkg.set_part("word/document.xml", xml.clone().into_bytes()));
    let mut doc = docxcore::load::parse_document_xml(&xml, &pkg.part_rels("word/document.xml"));
    let fitted = docxcore::import::html::import_html(fitted_html().as_bytes())
        .expect("the fitted table imports")
        .body
        .into_iter()
        .find(|b| matches!(b, docxcore::model::Block::Table(_)))
        .expect("a table");
    doc.body.insert(6, fitted);
    (doc, pkg)
}

/// The fixtures `ext-doc-load {"fixture"}` shows in the view (#1888):
/// `sections` ([`sections_document`]) and `header-inherit`
/// (`uiharness/fixtures/header-inherit.docx`, loaded as a document tab
/// loads it).
pub(crate) fn named_document(name: &str) -> Result<(Document, Package), String> {
    match name {
        "sections" => Ok(sections_document()),
        "header-inherit" => {
            let loaded = crate::load_bytes(include_bytes!(
                "../../../uiharness/fixtures/header-inherit.docx"
            ));
            let pkg = loaded.pkg.ok_or("header-inherit.docx did not load")?;
            Ok((loaded.doc, pkg))
        }
        _ => Err(format!(
            "unknown fixture '{name}': the fixtures are 'sections' and 'header-inherit'"
        )),
    }
}

/// A sectPr: the page size, its margins (`margin` all round, the header and
/// footer at half of it) and extra children (references first).
fn sect_pr(refs: &str, w: u32, h: u32, margin: u32, extra: &str) -> String {
    let orient = if w > h { " w:orient=\"landscape\"" } else { "" };
    let hf = margin / 2;
    format!(
        "<w:sectPr>{refs}<w:pgSz w:w=\"{w}\" w:h=\"{h}\"{orient}/>\
         <w:pgMar w:top=\"{margin}\" w:right=\"{margin}\" w:bottom=\"{margin}\" w:left=\"{margin}\" \
         w:header=\"{hf}\" w:footer=\"{hf}\" w:gutter=\"0\"/>{extra}</w:sectPr>"
    )
}

/// The `sections` document (#1888): four sections of different geometry,
/// for Print Layout. Section 1 is a Letter page (1" margins) with the
/// header "Sections header" and a footer whose text "Sections footer" has
/// a framed PAGE number at the margin's right beside it; the later
/// sections link to them. Section 2 (a new page) is landscape Letter with
/// 0.5" margins; section 3 (a new page) has two columns 0.5" apart with a
/// rule between them; section 4 follows it on the same page, one column,
/// so section 3's columns balance. Every word is unique: `Portrait`,
/// `Landscape`, `ColumnOne`..`ColumnSix`, `After`.
pub(crate) fn sections_document() -> (Document, Package) {
    let mut pkg = new_package(Document { body: vec![] });
    let (hdr, _) = pkg
        .create_hf_part(true, &para(&run("Sections header")))
        .expect("the header part is added");
    let page = "<w:pPr><w:framePr w:wrap=\"around\" w:vAnchor=\"text\" w:hAnchor=\"margin\" w:xAlign=\"right\" w:y=\"1\"/></w:pPr>\
                <w:r><w:fldChar w:fldCharType=\"begin\"/></w:r><w:r><w:instrText xml:space=\"preserve\"> PAGE </w:instrText></w:r>\
                <w:r><w:fldChar w:fldCharType=\"separate\"/></w:r><w:r><w:t>1</w:t></w:r><w:r><w:fldChar w:fldCharType=\"end\"/></w:r>";
    let (ftr, _) = pkg
        .create_hf_part(
            false,
            &format!("{}{}", para(page), para(&run("Sections footer"))),
        )
        .expect("the footer part is added");
    let refs = format!(
        "<w:headerReference w:type=\"default\" r:id=\"{hdr}\"/><w:footerReference w:type=\"default\" r:id=\"{ftr}\"/>"
    );
    let in_sect = |text: &str, sect: &str| para(&format!("<w:pPr>{sect}</w:pPr>{}", run(text)));
    let mut body = String::new();
    body += &in_sect("Portrait", &sect_pr(&refs, 12240, 15840, 1440, ""));
    body += &in_sect("Landscape", &sect_pr("", 15840, 12240, 720, ""));
    for i in [
        "ColumnOne",
        "ColumnTwo",
        "ColumnThree",
        "ColumnFour",
        "ColumnFive",
    ] {
        body += &para(&run(i));
    }
    body += &in_sect(
        "ColumnSix",
        &sect_pr(
            "",
            12240,
            15840,
            1440,
            "<w:cols w:num=\"2\" w:space=\"720\" w:sep=\"1\"/>",
        ),
    );
    body += &para(&run("After"));
    body += &sect_pr("", 12240, 15840, 1440, "<w:type w:val=\"continuous\"/>");
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:body>{body}</w:body></w:document>"
    );
    assert!(pkg.set_part("word/document.xml", xml.clone().into_bytes()));
    let doc = docxcore::load::parse_document_xml(&xml, &pkg.part_rels("word/document.xml"));
    (doc, pkg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use docxcore::editor::{Caret, Editor};
    use docxcore::model::{Block, Inline};

    #[test]
    fn the_fixture_document_has_a_link_a_table_and_a_resolving_picture() {
        let (doc, pkg) = fixture_document();
        // Heading, Alpha..., link, indented link, picture, table, the
        // fitted and the fixed table (#1895), 80 lines.
        assert_eq!(doc.body.len(), 88);
        assert!(matches!(doc.body[5], Block::Table(_)));
        let Block::Table(fitted) = &doc.body[6] else {
            panic!("the fitted table");
        };
        let tblw = docxcore::table::table_props(fitted);
        assert_eq!(tblw.attr("w:tblW", "w:type").as_deref(), Some("pct"));
        assert_eq!(tblw.attr("w:tblW", "w:w").as_deref(), Some("5000"));
        let cell_link = &fitted.rows[0].cells[0].blocks[0];
        let Block::Paragraph(cell_link) = cell_link else {
            panic!("a paragraph");
        };
        let at = cell_link.plain_text().find("cell link").expect("the link");
        assert_eq!(
            crate::doc_view::link_at(cell_link, at).as_deref(),
            Some(CELL_URL)
        );
        let Block::Table(fixed) = &doc.body[7] else {
            panic!("the fixed table");
        };
        assert_eq!(fixed.grid, [6000, 6000]);
        let Block::Paragraph(indented) = &doc.body[3] else {
            panic!("a paragraph");
        };
        assert_eq!(
            crate::doc_view::link_at(indented, 0).as_deref(),
            Some(INDENTED_URL)
        );
        assert_eq!(indented.props.indent, 1440);
        let Block::Paragraph(link) = &doc.body[2] else {
            panic!("a paragraph");
        };
        assert_eq!(
            crate::doc_view::link_at(link, "Follow the ".len()).as_deref(),
            Some(LINK_URL)
        );
        let Block::Paragraph(pic) = &doc.body[4] else {
            panic!("a paragraph");
        };
        let raw = pic
            .content
            .iter()
            .find_map(|i| match i {
                Inline::Raw(r) => Some(r.clone()),
                _ => None,
            })
            .expect("the picture run");
        let rid = docxcore::image::inline_embed(&raw).expect("an inline picture");
        let (ext, _) = crate::pictures::resolve(&pkg, "word/document.xml", &rid).expect("resolves");
        assert_eq!(ext, "png");
        // A table cell's paragraph is reached by its path.
        let a2 = docxcore::editor::resolve_para(&doc.body, &[5, 1, 0, 0]).expect("A2");
        assert_eq!(a2.plain_text(), "A2");
    }

    #[test]
    fn the_fixture_ribbon_flips_its_toggle_and_gallery_and_records_ids() {
        let state = RibbonState::default();
        let r = crate::ext_ribbon::convert(&fixture_ribbon(&state));
        assert_eq!(r.tab_names(), vec![("Message", "M"), ("View", "V")]);
        assert!(!r.checked("fx.flag") && !r.enabled("fx.archive"));
        assert!(r.checked("fx.density.compact"));
        run_ribbon_action(&state, "fx.flag");
        run_ribbon_action(&state, "fx.density.roomy");
        state.archive_enabled.set(true);
        let r = crate::ext_ribbon::convert(&fixture_ribbon(&state));
        assert!(r.checked("fx.flag") && r.enabled("fx.archive"));
        assert!(r.checked("fx.density.roomy") && !r.checked("fx.density.compact"));
        assert_eq!(*state.actions.borrow(), ["fx.flag", "fx.density.roomy"]);
    }

    #[test]
    fn a_selection_of_the_fixture_copies_its_text_and_bold_in_the_rtf() {
        let (doc, pkg) = fixture_document();
        let mut ed = Editor::new(doc);
        ed.anchor = Some(Caret::at(vec![1], 0));
        ed.caret = Caret::at(vec![1], "Alpha Beta Gamma".len());
        let (text, rtf) = crate::doc_view::copy_payload(&ed, Some(&pkg)).expect("a copy");
        assert_eq!(text, "Alpha Beta Gamma");
        let rtf = rtf.expect("RTF beside the text");
        assert!(rtf.contains("\\b"), "{rtf}");
        assert!(rtf.contains("Beta"), "{rtf}");
        // Nothing selected copies nothing.
        ed.anchor = None;
        assert!(crate::doc_view::copy_payload(&ed, Some(&pkg)).is_none());
        // Select All copies everything, the last line included.
        ed.select_all();
        let (all, _) = crate::doc_view::copy_payload(&ed, Some(&pkg)).expect("a copy");
        assert!(all.starts_with("Embedded view"), "{all}");
        assert!(all.trim_end().ends_with("Line 80"), "{all}");
    }
}
