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

/// A File page of the fixture's (#1923): its title, a Close tab button, a
/// button showing the other page (#2204), how many times it rendered and
/// where it and the buttons last painted (the harness's `ext-page-read` and
/// `ext-page-close:<id>` / `ext-page-show:<id>` regions).
pub(crate) struct FixturePage {
    title: &'static str,
    /// The other page, which this page's Show button opens through
    /// `ext::ShowBackstagePage`: its id and title.
    show: (&'static str, &'static str),
    /// The Show button's own focus, which a click on it takes, as a
    /// gpui-component Button's does (#2204 r1).
    show_focus: FocusHandle,
    pub(crate) renders: Rc<Cell<usize>>,
    pub(crate) bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(crate) close_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub(crate) show_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
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
            .child({
                let (id, title) = self.show;
                div()
                    .id("ext-page-show")
                    .track_focus(&self.show_focus)
                    .relative()
                    .px_3()
                    .py_1()
                    .border_1()
                    .border_color(rgb(0x808080))
                    .rounded_sm()
                    .cursor_pointer()
                    .child(record_bounds(&self.show_bounds))
                    .child(format!("Show {title}"))
                    .on_click(move |_, window, cx| {
                        let id = id.to_string();
                        window.dispatch_action(Box::new(ext::ShowBackstagePage { id }), cx);
                    })
            })
    }
}

/// The fixture's File pages (#1923): Fixture Account at the top (with the
/// Reply icon), Fixture Options at the bottom.
fn fixture_pages(cx: &mut App) -> Vec<BackstagePage> {
    let mut page = |(id, title): (&'static str, &'static str), show| {
        let view = cx.new(|cx| FixturePage {
            title,
            show,
            show_focus: cx.focus_handle(),
            renders: Rc::default(),
            bounds: Rc::default(),
            close_bounds: Rc::default(),
            show_bounds: Rc::default(),
        });
        BackstagePage::new(id, title, view)
    };
    // Each page's Show button opens the other one (#2204).
    vec![
        page(ACCOUNT_PAGE, OPTIONS_PAGE).icon(RibbonIcon::svg(REPLY_SVG.as_bytes())),
        page(OPTIONS_PAGE, ACCOUNT_PAGE).group(BackstageGroup::Bottom),
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
/// `sections` ([`sections_document`]), `header-inherit`
/// (`uiharness/fixtures/header-inherit.docx`, loaded as a document tab
/// loads it), `theme-font` ([`theme_font_docx`], #1972) and `word-compare`
/// ([`word_compare_docx`], #2231).
pub(crate) fn named_document(name: &str) -> Result<(Document, Package), String> {
    match name {
        "sections" => Ok(sections_document()),
        "theme-font" => {
            let loaded = crate::load_bytes(&theme_font_docx());
            let pkg = loaded.pkg.ok_or("theme-font did not load")?;
            Ok((loaded.doc, pkg))
        }
        "word-compare" => {
            let loaded = crate::load_bytes(&word_compare_docx());
            let pkg = loaded.pkg.ok_or("word-compare did not load")?;
            Ok((loaded.doc, pkg))
        }
        "header-inherit" => {
            let loaded = crate::load_bytes(include_bytes!(
                "../../../uiharness/fixtures/header-inherit.docx"
            ));
            let pkg = loaded.pkg.ok_or("header-inherit.docx did not load")?;
            Ok((loaded.doc, pkg))
        }
        _ => Err(format!(
            "unknown fixture '{name}': the fixtures are 'sections', 'header-inherit', 'theme-font' and 'word-compare'"
        )),
    }
}

/// The `theme-font` styles (#1972), the shape of the owner's file:
/// `docDefaults` naming only the theme's minor font, a default Normal with
/// no run properties.
pub(crate) const THEME_FONT_STYLES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme=\"minorHAnsi\" w:eastAsiaTheme=\"minorHAnsi\" \
w:hAnsiTheme=\"minorHAnsi\" w:cstheme=\"minorBidi\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr><w:spacing w:after=\"160\" w:line=\"259\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
</w:styles>";

/// The `theme-font` theme: minor (body) Calibri, major Calibri Light.
pub(crate) const THEME_FONT_THEME: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Office Theme\"><a:themeElements>\
<a:fontScheme name=\"Office\"><a:majorFont><a:latin typeface=\"Calibri Light\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont>\
<a:minorFont><a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme>\
</a:themeElements></a:theme>";

/// The `theme-font` document (#1972), as the bytes of a `.docx` (also
/// `uiharness/fixtures/theme-font.docx`, which a test keeps equal to it):
/// [`THEME_FONT_STYLES`] and [`THEME_FONT_THEME`], and one paragraph with
/// no `w:pStyle` whose runs name no font: "Plain ", "Strong" with only
/// `w:b`, " " and "Slanted" with only `w:i`.
pub(crate) fn theme_font_docx() -> Vec<u8> {
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/word/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    let doc_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"theme/theme1.xml\"/></Relationships>";
    let body = para(&format!(
        "{}<w:r><w:rPr><w:b/></w:rPr><w:t>Strong</w:t></w:r>{}<w:r><w:rPr><w:i/></w:rPr><w:t>Slanted</w:t></w:r>",
        run("Plain "),
        run(" ")
    ));
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:body>{body}{}</w:body></w:document>",
        sect_pr("", 12240, 15840, 1440, "")
    );
    let part = |n: &str, t: &str| (n.to_string(), t.as_bytes().to_vec());
    docxcore::zipwrite::write_zip(&[
        part("[Content_Types].xml", ct),
        part("_rels/.rels", rels),
        part("word/document.xml", &document),
        part("word/_rels/document.xml.rels", doc_rels),
        part("word/styles.xml", THEME_FONT_STYLES),
        part("word/theme/theme1.xml", THEME_FONT_THEME),
    ])
}

/// The `word-compare` styles (#2231): a Calibri Normal, a `Title` with the
/// issue's bottom rule (`w:sz="8" w:space="4"`, `4F81BD` and
/// `w:themeColor="accent1"`) and a `ListParagraph` indented 720.
pub(crate) const WORD_COMPARE_STYLES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:asciiTheme=\"minorHAnsi\" w:hAnsiTheme=\"minorHAnsi\"/>\
<w:sz w:val=\"22\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"160\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/>\
<w:qFormat/><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"8\" w:space=\"4\" w:color=\"4F81BD\" w:themeColor=\"accent1\"/></w:pBdr>\
<w:spacing w:after=\"300\"/></w:pPr><w:rPr><w:rFonts w:asciiTheme=\"majorHAnsi\" w:hAnsiTheme=\"majorHAnsi\"/>\
<w:color w:val=\"17365D\"/><w:sz w:val=\"52\"/></w:rPr></w:style>\
<w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/>\
<w:qFormat/><w:pPr><w:ind w:left=\"720\"/><w:contextualSpacing/></w:pPr></w:style>\
</w:styles>";

/// The `word-compare` theme: Calibri / Calibri Light, accent 1 `4F81BD`.
pub(crate) const WORD_COMPARE_THEME: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<a:theme xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" name=\"Office Theme\"><a:themeElements>\
<a:clrScheme name=\"Office\"><a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/></a:dk1>\
<a:lt1><a:sysClr val=\"window\" lastClr=\"FFFFFF\"/></a:lt1><a:dk2><a:srgbClr val=\"1F497D\"/></a:dk2>\
<a:lt2><a:srgbClr val=\"EEECE1\"/></a:lt2><a:accent1><a:srgbClr val=\"4F81BD\"/></a:accent1>\
<a:accent2><a:srgbClr val=\"C0504D\"/></a:accent2><a:accent3><a:srgbClr val=\"9BBB59\"/></a:accent3>\
<a:accent4><a:srgbClr val=\"8064A2\"/></a:accent4><a:accent5><a:srgbClr val=\"4BACC6\"/></a:accent5>\
<a:accent6><a:srgbClr val=\"F79646\"/></a:accent6><a:hlink><a:srgbClr val=\"0000FF\"/></a:hlink>\
<a:folHlink><a:srgbClr val=\"800080\"/></a:folHlink></a:clrScheme>\
<a:fontScheme name=\"Office\"><a:majorFont><a:latin typeface=\"Calibri Light\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont>\
<a:minorFont><a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont></a:fontScheme>\
</a:themeElements></a:theme>";

/// The `word-compare` numbering: list 1, a Symbol bullet whose level 0 is
/// indented `w:left="720" w:hanging="360"`, as Word's own bullets are.
pub(crate) const WORD_COMPARE_NUMBERING: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:numbering xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:abstractNum w:abstractNumId=\"0\"><w:multiLevelType w:val=\"hybridMultilevel\"/>\
<w:lvl w:ilvl=\"0\"><w:start w:val=\"1\"/><w:numFmt w:val=\"bullet\"/><w:lvlText w:val=\"&#xF0B7;\"/><w:lvlJc w:val=\"left\"/>\
<w:pPr><w:ind w:left=\"720\" w:hanging=\"360\"/></w:pPr><w:rPr><w:rFonts w:ascii=\"Symbol\" w:hAnsi=\"Symbol\" w:hint=\"default\"/></w:rPr></w:lvl>\
</w:abstractNum><w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num></w:numbering>";

/// The `word-compare` document (#2231), as the bytes of a `.docx` (also
/// `uiharness/fixtures/word-compare.docx`, which a test keeps equal to it):
/// the four shapes of the owner's comparison with Word, in made-up text.
/// Block 0 is the `Title` (its style's accent rule); blocks 1 and 2 are
/// `ListParagraph` bullets of list 1 (mark in Segoe UI, text in Arial; 2
/// wraps); block 3 has an explicit `212121` yellow-highlighted run
/// "Explicit" and an automatic one "Automatic"; block 4 is all highlighted
/// at 1.15 lines and wraps; blocks 5 and 6 share a direct box border
/// (top, left, bottom, right and between, `C0504D`).
pub(crate) fn word_compare_docx() -> Vec<u8> {
    let ct = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>\
<Override PartName=\"/word/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/></Types>";
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    let doc_rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>\
<Relationship Id=\"rId3\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/theme\" Target=\"theme/theme1.xml\"/></Relationships>";
    let arial = |text: &str| {
        format!(
            "<w:r><w:rPr><w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\"/></w:rPr>\
             <w:t xml:space=\"preserve\">{text}</w:t></w:r>"
        )
    };
    let bullet = |text: &str| {
        format!(
            "<w:p><w:pPr><w:pStyle w:val=\"ListParagraph\"/><w:numPr><w:ilvl w:val=\"0\"/><w:numId w:val=\"1\"/></w:numPr>\
             <w:rPr><w:rFonts w:ascii=\"Segoe UI\" w:hAnsi=\"Segoe UI\"/></w:rPr></w:pPr>{}</w:p>",
            arial(text)
        )
    };
    let hl = |color: Option<&str>, text: &str| {
        let color = color.map_or(String::new(), |c| format!("<w:color w:val=\"{c}\"/>"));
        format!(
            "<w:r><w:rPr>{color}<w:highlight w:val=\"yellow\"/></w:rPr><w:t xml:space=\"preserve\">{text}</w:t></w:r>"
        )
    };
    let boxed = |text: &str| {
        let side = |tag: &str, space: u32| {
            format!("<w:{tag} w:val=\"single\" w:sz=\"6\" w:space=\"{space}\" w:color=\"C0504D\"/>")
        };
        format!(
            "<w:p><w:pPr><w:pBdr>{}{}{}{}{}</w:pBdr></w:pPr>{}</w:p>",
            side("top", 1),
            side("left", 4),
            side("bottom", 1),
            side("right", 4),
            side("between", 1),
            run(text)
        )
    };
    let body = [
        "<w:p><w:pPr><w:pStyle w:val=\"Title\"/></w:pPr><w:r><w:t>Quarterly Lorem Notes</w:t></w:r></w:p>".to_string(),
        bullet("Alpha lorem ipsum dolor sit amet."),
        bullet(
            "Beta consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore \
             magna aliqua, ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut \
             aliquip ex ea commodo consequat.",
        ),
        format!(
            "<w:p>{}{}{}</w:p>",
            hl(Some("212121"), "Explicit"),
            run(" and "),
            hl(None, "Automatic")
        ),
        format!(
            "<w:p><w:pPr><w:spacing w:line=\"276\" w:lineRule=\"auto\"/></w:pPr>{}</w:p>",
            hl(
                None,
                "Gamma duis aute irure dolor in reprehenderit in voluptate velit esse cillum dolore eu \
                 fugiat nulla pariatur, excepteur sint occaecat cupidatat non proident, sunt in culpa qui \
                 officia deserunt mollit anim id est laborum, sed ut perspiciatis unde omnis iste natus."
            )
        ),
        boxed("Delta boxed lorem one."),
        boxed("Epsilon boxed lorem two."),
    ]
    .concat();
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:body>{body}{}</w:body></w:document>",
        sect_pr("", 12240, 15840, 1440, "")
    );
    let part = |n: &str, t: &str| (n.to_string(), t.as_bytes().to_vec());
    docxcore::zipwrite::write_zip(&[
        part("[Content_Types].xml", ct),
        part("_rels/.rels", rels),
        part("word/document.xml", &document),
        part("word/_rels/document.xml.rels", doc_rels),
        part("word/styles.xml", WORD_COMPARE_STYLES),
        part("word/numbering.xml", WORD_COMPARE_NUMBERING),
        part("word/theme/theme1.xml", WORD_COMPARE_THEME),
    ])
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

/// One comment of the `comments` fixture (#2232; test-only: the UI case
/// opens the files it wrote): its id, author, date,
/// whether it is resolved and its text.
#[cfg(test)]
struct FixtureComment {
    id: u32,
    author: &'static str,
    date: &'static str,
    done: bool,
    text: &'static str,
}

/// The `comments` fixture's comments (#2232): three open and two resolved,
/// by two made-up reviewers. The open ones are dated at noon UTC, so their
/// day is November 17 from 11 h behind UTC to 11 h ahead.
#[cfg(test)]
const FIXTURE_COMMENTS: [FixtureComment; 5] = [
    FixtureComment {
        id: 0,
        author: "Avery Lane",
        date: "2026-11-16T09:00:00Z",
        done: true,
        text: "Settled: the title stays.",
    },
    FixtureComment {
        id: 1,
        author: "Avery Lane",
        date: "2026-11-17T12:00:00Z",
        done: false,
        text: "Is this the right word?",
    },
    FixtureComment {
        id: 2,
        author: "Robin Hale",
        date: "2026-11-17T12:05:00Z",
        done: false,
        text: "And this one, next to it?",
    },
    FixtureComment {
        id: 3,
        author: "Robin Hale",
        date: "2026-11-17T12:10:00Z",
        done: false,
        text: "This range runs across two paragraphs, and this remark is long enough to \
               wrap over several lines of its balloon, so the balloon reaches below the last \
               line of the document and the view must make room under it to scroll it into \
               sight.",
    },
    FixtureComment {
        id: 4,
        author: "Robin Hale",
        date: "2026-11-16T10:30:00Z",
        done: true,
        text: "Done, thanks.",
    },
];

/// The `comments` document (#2232) as `.docx` bytes, and with
/// `with_comments` false the same text with no comment at all (the
/// `comments-plain` twin the text layout is compared with). Six made-up
/// paragraphs; comment 0 (resolved) on "Quarterly" in block 0; 1 and 2 on
/// the adjacent words "near" and "words" of block 2's one short line (two
/// anchors on one line: the second balloon is pushed down); 3's range from
/// "range", the last word of block 3 (which wraps in both views), to "ends"
/// in block 4 (across two paragraphs), its text long; 4 (resolved) on
/// "closing" in block 5.
#[cfg(test)]
pub(crate) fn comments_docx(with_comments: bool) -> Vec<u8> {
    let start = |id: u32| {
        if with_comments {
            format!("<w:commentRangeStart w:id=\"{id}\"/>")
        } else {
            String::new()
        }
    };
    let end = |id: u32| {
        if with_comments {
            format!(
                "<w:commentRangeEnd w:id=\"{id}\"/><w:r><w:rPr><w:rStyle w:val=\"CommentReference\"/>\
                 </w:rPr><w:commentReference w:id=\"{id}\"/></w:r>"
            )
        } else {
            String::new()
        }
    };
    let body = [
        para(&format!(
            "{}{}{}{}",
            start(0),
            run("Quarterly"),
            end(0),
            run(" lorem notes")
        )),
        para(&run("Alpha lorem ipsum dolor sit amet.")),
        para(&format!(
            "{}{}{}{}{}{}{}",
            run("Beta "),
            start(1),
            run("near"),
            end(1),
            run(" "),
            start(2),
            run("words") + &end(2) + &run(" here.")
        )),
        para(&format!(
            "{}{}{}",
            run(
                "Gamma lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod \
                 tempor incididunt ut labore et dolore magna aliqua, and then it starts a "
            ),
            start(3),
            run("range")
        )),
        para(&format!(
            "{}{}",
            run("Delta ends"),
            end(3) + &run(" it here.")
        )),
        para(&format!(
            "{}{}{}{}",
            run("Epsilon "),
            start(4),
            run("closing"),
            end(4) + &run(" line.")
        )),
    ]
    .concat();
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\"><w:body>{body}{}</w:body></w:document>",
        sect_pr("", 12240, 15840, 1440, "")
    );
    let comment_parts = if with_comments {
        "<Override PartName=\"/word/comments.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml\"/>\
<Override PartName=\"/word/commentsExtended.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.commentsExtended+xml\"/>"
    } else {
        ""
    };
    let ct = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
{comment_parts}</Types>"
    );
    let rels = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>";
    let doc_rels = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{}</Relationships>",
        if with_comments {
            "<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments\" Target=\"comments.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.microsoft.com/office/2011/relationships/commentsExtended\" Target=\"commentsExtended.xml\"/>"
        } else {
            ""
        }
    );
    let para_id = |id: u32| format!("1000000{id}");
    let comments: String = FIXTURE_COMMENTS
        .iter()
        .map(|c| {
            let initials = docxcore::comments::initials(c.author);
            format!(
                "<w:comment w:id=\"{}\" w:author=\"{}\" w:date=\"{}\" w:initials=\"{initials}\">\
                 <w:p w14:paraId=\"{}\"><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p></w:comment>",
                c.id,
                c.author,
                c.date,
                para_id(c.id),
                c.text
            )
        })
        .collect();
    let comments = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:comments xmlns:w=\"{W}\" xmlns:w14=\"http://schemas.microsoft.com/office/word/2010/wordml\">{comments}</w:comments>"
    );
    let extended: String = FIXTURE_COMMENTS
        .iter()
        .map(|c| {
            format!(
                "<w15:commentEx w15:paraId=\"{}\" w15:done=\"{}\"/>",
                para_id(c.id),
                u8::from(c.done)
            )
        })
        .collect();
    let extended = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w15:commentsEx xmlns:w15=\"http://schemas.microsoft.com/office/word/2012/wordml\">{extended}</w15:commentsEx>"
    );
    let part = |n: &str, t: &str| (n.to_string(), t.as_bytes().to_vec());
    let mut parts = vec![
        part("[Content_Types].xml", &ct),
        part("_rels/.rels", rels),
        part("word/document.xml", &document),
        part("word/_rels/document.xml.rels", &doc_rels),
    ];
    if with_comments {
        parts.push(part("word/comments.xml", &comments));
        parts.push(part("word/commentsExtended.xml", &extended));
    }
    docxcore::zipwrite::write_zip(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    use docxcore::editor::{Caret, Editor};
    use docxcore::model::{Block, Inline};

    /// `uiharness/fixtures/comments.docx` and `comments-plain.docx` are
    /// [`comments_docx`]'s bytes (#2232); `UPDATE_COMMENTS_FIXTURE=1` writes
    /// them. The first loads with three open and two resolved comments, all
    /// anchored in the body; the second with the same text and none.
    #[test]
    fn the_comments_fixture_files_are_current_2232() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../uiharness/fixtures");
        for (name, with) in [("comments.docx", true), ("comments-plain.docx", false)] {
            let path = dir.join(name);
            let bytes = comments_docx(with);
            if std::env::var_os("UPDATE_COMMENTS_FIXTURE").is_some() {
                std::fs::write(&path, &bytes).expect("write the fixture");
            }
            let on_disk = std::fs::read(&path).expect(name);
            assert!(
                on_disk == bytes,
                "{name} is stale: run with UPDATE_COMMENTS_FIXTURE=1"
            );
        }
        let with = crate::load_bytes(&comments_docx(true));
        let plain = crate::load_bytes(&comments_docx(false));
        let pkg = with.pkg.expect("a package");
        let comments = docxcore::comments::parse_comments(&pkg);
        let open: Vec<&str> = comments
            .iter()
            .filter(|c| !c.resolved)
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(open, vec!["1", "2", "3"]);
        assert_eq!(comments.iter().filter(|c| c.resolved).count(), 2);
        let anchored = docxcore::inspect::comment_marker_ids(&with.doc);
        assert_eq!(anchored.len(), 5);
        let text = |d: &docxcore::model::Document| -> Vec<String> {
            d.body
                .iter()
                .filter_map(|b| match b {
                    Block::Paragraph(p) => Some(p.plain_text()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(text(&with.doc), text(&plain.doc));
        assert_eq!(text(&with.doc)[2], "Beta near words here.");
        // Comment 3's range is open at block 4's start.
        let body = crate::balloons::body_comments(&with.doc.body);
        assert_eq!(body.open_at.get(&vec![4]), Some(&vec!["3".to_string()]));
    }

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
            crate::doc_view::link_at(cell_link, at, &pkg.document_rels()).as_deref(),
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
            crate::doc_view::link_at(indented, 0, &pkg.document_rels()).as_deref(),
            Some(INDENTED_URL)
        );
        assert_eq!(indented.props.indent, 1440);
        let Block::Paragraph(link) = &doc.body[2] else {
            panic!("a paragraph");
        };
        assert_eq!(
            crate::doc_view::link_at(link, "Follow the ".len(), &pkg.document_rels()).as_deref(),
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

    /// `uiharness/fixtures/word-compare.docx` is [`word_compare_docx`]'s
    /// bytes (#2231). `UPDATE_WORD_COMPARE_FIXTURE=1` writes it. It loads
    /// with its four shapes where the case reads them.
    #[test]
    fn the_word_compare_fixture_file_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../uiharness/fixtures/word-compare.docx");
        let bytes = word_compare_docx();
        if std::env::var_os("UPDATE_WORD_COMPARE_FIXTURE").is_some() {
            std::fs::write(&path, &bytes).expect("write the fixture");
        }
        let on_disk = std::fs::read(&path).expect("uiharness/fixtures/word-compare.docx");
        assert!(
            on_disk == bytes,
            "word-compare.docx is stale: run with UPDATE_WORD_COMPARE_FIXTURE=1"
        );
        let loaded = crate::load_bytes(&bytes);
        let pkg = loaded.pkg.expect("a package");
        let para = |i: usize| match &loaded.doc.body[i] {
            Block::Paragraph(p) => p.clone(),
            b => panic!("block {i}: {b:?}"),
        };
        let sheet = docxcore::styles::parse_styles_xml(&pkg.part_text("word/styles.xml").unwrap());
        let title = sheet.effective_borders(Some("Title"), para(0).props.borders);
        let rule = title.bottom.expect("the title's rule");
        assert_eq!((rule.sz, rule.space, rule.color), (8, 4, Some(0x4F81BD)));
        let colors =
            docxcore::theme::parse_theme_colors(&pkg.part_text("word/theme/theme1.xml").unwrap());
        assert_eq!(rule.shown_color(Some(&colors)), Some(0x4F81BD));
        let numbering =
            docxcore::numbering::parse_numbering_xml(&pkg.part_text("word/numbering.xml").unwrap());
        let item = para(1).props;
        let e = crate::eff_indent(
            &item,
            crate::Inherited::of(&item, Some(&sheet), Some(&numbering)),
        );
        assert_eq!((e.left, e.first), (720, -360));
        assert_eq!(para(3).plain_text(), "Explicit and Automatic");
        assert!(para(5).props.borders.between.is_some());
        assert_eq!(para(5).props.borders, para(6).props.borders);
    }

    /// `uiharness/fixtures/theme-font.docx` is [`theme_font_docx`]'s bytes,
    /// so the main tab's case and the view's fixture cannot drift (#1972).
    /// `UPDATE_THEME_FONT_FIXTURE=1` writes it.
    #[test]
    fn the_theme_font_fixture_file_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../uiharness/fixtures/theme-font.docx");
        let bytes = theme_font_docx();
        if std::env::var_os("UPDATE_THEME_FONT_FIXTURE").is_some() {
            std::fs::write(&path, &bytes).expect("write the fixture");
        }
        let on_disk = std::fs::read(&path).expect("uiharness/fixtures/theme-font.docx");
        assert!(
            on_disk == bytes,
            "theme-font.docx is stale: run with UPDATE_THEME_FONT_FIXTURE=1"
        );
        let (doc, pkg) = named_document("theme-font").expect("loads");
        let Block::Paragraph(p) = &doc.body[0] else {
            panic!("a paragraph");
        };
        assert_eq!(p.plain_text(), "Plain Strong Slanted");
        assert_eq!(p.props.style_id, None);
        assert_eq!(
            pkg.theme_part_name().as_deref(),
            Some("word/theme/theme1.xml")
        );
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
        let (text, rtf) = crate::clip_payload(&ed.copy().expect("a copy"), Some(&pkg));
        assert_eq!(text, "Alpha Beta Gamma");
        let rtf = rtf.expect("RTF beside the text");
        assert!(rtf.contains("\\b"), "{rtf}");
        assert!(rtf.contains("Beta"), "{rtf}");
        // Nothing selected copies nothing.
        ed.anchor = None;
        assert!(ed.copy().is_none());
        // Select All copies everything, the last line included.
        ed.select_all();
        let (all, _) = crate::clip_payload(&ed.copy().expect("a copy"), Some(&pkg));
        assert!(all.starts_with("Embedded view"), "{all}");
        assert!(all.trim_end().ends_with("Line 80"), "{all}");
    }
}
