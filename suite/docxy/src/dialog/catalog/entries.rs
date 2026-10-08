//! The catalogue's entries (#1029); see [`super`].
//!
//! `open` is how a person reaches each dialog, as script lines run after the
//! fixture is open. The steps that only set up the document (`type`,
//! `key`, `click`) and the openers (`ribbon-click`, `menu-click`,
//! `dialog-click` on a parent) are the harness's handler verbs; every key
//! and click into the dialog after that is real input.

use super::*;

const DOCX: &str = "../fixtures/basic.docx";
const XLSX: &str = "../fixtures/basic.xlsx";
const TABLE_DOCX: &str = "../fixtures/doc-table.docx";

const fn dropdown(name: &'static str) -> Field {
    Field::other(name, ControlKind::Dropdown)
}

const fn radio(name: &'static str) -> Field {
    Field::other(name, ControlKind::Radio)
}

const fn checkbox(name: &'static str) -> Field {
    Field::other(name, ControlKind::Checkbox)
}

/// Tab to a radio group or dropdown and step it `down` items, as a person
/// picks a choice that shows or enables another control.
macro_rules! pick {
    ($field:literal, $down:literal) => {
        &[
            concat!(
                r#"call real-key {"key":"tab","to-field":""#,
                $field,
                r#""}"#
            ),
            concat!(r#"call real-key {"key":"down","times":"#, $down, "}"),
        ]
    };
}

// Prep: the choices that show or enable a field.
const TWELVE_COLUMNS: &[&str] = &[
    r#"call pointer-click {"dialog-field":"num"}"#,
    r#"call real-key {"key":"ctrl+a"}"#,
    r#"call real-type {"text":"12"}"#,
    r#"call pointer-click {"dialog-control":"equal"}"#,
];
const CHAPTER_ON: &[&str] = &[r#"call pointer-click {"dialog-control":"chapter"}"#];
const START_AT: &[&str] = pick!("numbering", 1);
const SEPARATE_WITH_OTHER: &[&str] = pick!("sep", 2);
const TWO_COLORS: &[&str] = pick!("colors", 1);
const TEXT_WATERMARK: &[&str] = pick!("kind", 1);
const CUSTOM_LABEL: &[&str] = pick!("product", 10);
const RECORDS_FROM: &[&str] = pick!("records", 2);
const WHOLE_NUMBER: &[&str] = pick!("allow", 1);
const WHOLE_NUMBER_MAX: &[&str] = &[
    r#"call real-key {"key":"tab","to-field":"allow"}"#,
    r#"call real-key {"key":"down"}"#,
    r#"call pointer-click {"dialog-field":"second"}"#,
    r#"call real-type {"text":"90"}"#,
];
const WHOLE_NUMBER_MIN: &[&str] = &[
    r#"call real-key {"key":"tab","to-field":"allow"}"#,
    r#"call real-key {"key":"down"}"#,
    r#"call pointer-click {"dialog-field":"first"}"#,
    r#"call real-type {"text":"10"}"#,
];
const FIXED_WIDTH: &[&str] = pick!("kind", 1);
const TWO_COLUMNS: &[&str] = &[
    r#"call pointer-click {"dialog-tab":"Step 2: Delimiters"}"#,
    r#"call pointer-click {"dialog-control":"comma"}"#,
];
const CRITERIA: &[&str] = &[
    r#"call pointer-click {"dialog-field":"criteria"}"#,
    r#"call real-type {"text":"E1:E2"}"#,
];

const STEP2: &str = "Step 2: Delimiters";
const STEP3: &str = "Step 3: Column data format";

// Why a case does not press OK with the sample typed.
const COLUMN_FIT: &str =
    "OK refuses widths that do not fill the text width, which one width alone cannot";
const LABEL_SIZE: &str = "a label's size is in no state a script reads; across and down are";
const SEPARATORS: &str =
    "the separators only change how numbers parse, and the wizard's column here is text";
const THROUGH_ADD: &str = "the value takes effect through Add, not OK";
const SORT_LIST: &str = "its list is read only with Order: Custom List... chosen";

// What OK shows.
const ENVELOPE_ADDED: &str = "assert status is Envelope added: Size 10 (4 1/8 x 9 1/2 in)";
const VALIDATION_APPLIED: &str = "assert status is Data validation applied";

dialogs! {
    // ---- Word: Layout, Header & Footer, Insert -----------------------------
    PAGE_SETUP = "page-setup" {
        surface: Surface::Doc,
        file: "doc-layout",
        fixture: DOCX,
        open: &[
            r#"call menu-open {"target":{"ribbon":["Layout","Page Setup","Margins"]}}"#,
            r#"call menu-click {"label":"Custom Margins..."}"#,
        ],
        fields: &[
            Field::number("top", "1.5").on("Margins").kept("1.5"),
            Field::number("bottom", "1.5").on("Margins").kept("1.5"),
            Field::number("left", "1.5").on("Margins").kept("1.5"),
            Field::number("right", "1.5").on("Margins").kept("1.5"),
            Field::number("gutter", "0.5").on("Margins").kept("0.5"),
            radio("gutter_pos").on("Margins"),
            radio("orientation").on("Margins"),
            dropdown("multiple").on("Margins"),
            dropdown("paper").on("Paper"),
            Field::number("width", "8").on("Paper").kept("8"),
            Field::number("height", "10").on("Paper").kept("10"),
            dropdown("start").on("Layout"),
            Field::number("header", "0.6").on("Layout").kept("0.6"),
            Field::number("footer", "0.6").on("Layout").kept("0.6"),
            dropdown("apply"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    COLUMNS = "columns" {
        surface: Surface::Doc,
        file: "doc-layout",
        fixture: DOCX,
        open: &[
            r#"call menu-open {"target":{"ribbon":["Layout","Page Setup","Columns"]}}"#,
            r#"call menu-click {"label":"More Columns..."}"#,
        ],
        fields: &[
            radio("preset"),
            Field::number("num", "2").kept("2"),
            Field::number("width1", "6").no_ok(COLUMN_FIT),
            Field::number("space1", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width2", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space2", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width3", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space3", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width4", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space4", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width5", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space5", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width6", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space6", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width7", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space7", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width8", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space8", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width9", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space9", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width10", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space10", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width11", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space11", "0.1").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("width12", "0.4").prep(TWELVE_COLUMNS).no_ok(COLUMN_FIT),
            Field::number("space12", "0.1").skip("never shown: no gap follows the twelfth column"),
            checkbox("equal"),
            checkbox("sep"),
            dropdown("apply"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    HF_DISTANCE = "hf-distance" {
        surface: Surface::Doc,
        file: "doc-layout",
        fixture: DOCX,
        open: &[
            r#"call ribbon-click {"tab":"Insert","command":"Edit Header"}"#,
            r#"call ribbon-click {"tab":"Header & Footer","command":"hf-top"}"#,
            r#"call menu-click {"label":"Custom..."}"#,
        ],
        fields: &[
            Field::number("distance", "0.75")
                .applied(&["call hf-state {}", "assert reply.header_from_top is 0.75"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    PAGE_NUMBER_FORMAT = "page-number-format" {
        surface: Surface::Doc,
        file: "doc-layout",
        fixture: DOCX,
        open: &[r#"call ribbon-click {"tab":"Insert","command":"pn-format"}"#],
        fields: &[
            dropdown("format"),
            checkbox("chapter"),
            dropdown("chap_style").prep(CHAPTER_ON),
            dropdown("chap_sep").prep(CHAPTER_ON),
            radio("numbering"),
            Field::number("start", "5").prep(START_AT).kept("5"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    // ---- Word: tables ------------------------------------------------------
    INSERT_TABLE = "insert-table" {
        surface: Surface::Doc,
        file: "doc-tables",
        fixture: DOCX,
        open: &[
            r#"call ribbon-click {"tab":"Insert","command":"table"}"#,
            r#"call menu-click {"label":"Insert Table..."}"#,
        ],
        fields: &[
            Field::number("cols", "3").applied(&["assert table.columns is 3"]),
            Field::number("rows", "4").applied(&["assert table.rows is 4"]),
            radio("fit"),
            Field::text("width")
                .sample("1.5")
                .no_ok("no state a script reads reports a table's column width"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    DELETE_CELLS = "delete-cells" {
        surface: Surface::Doc,
        file: "doc-tables",
        fixture: TABLE_DOCX,
        open: &[
            r#"call selection-set {"start":7,"end":7}"#,
            r#"call ribbon-click {"tab":"Table Layout","command":"delete-cells"}"#,
        ],
        fields: &[
            radio("shift"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    SPLIT_CELLS = "split-cells" {
        surface: Surface::Doc,
        file: "doc-tables",
        fixture: TABLE_DOCX,
        open: &[
            r#"call selection-set {"start":7,"end":7}"#,
            r#"call ribbon-click {"tab":"Table Layout","command":"split-cells"}"#,
        ],
        fields: &[
            // Cell A of the 1x2 table, split into 3 (the dialog opens on 2).
            Field::number("cols", "3").applied(&["assert table.columns is 4"]),
            Field::number("rows", "2").applied(&["assert table.rows is 2"]),
            checkbox("merge"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    TABLE_SORT = "sort" {
        surface: Surface::Doc,
        file: "doc-tables",
        fixture: TABLE_DOCX,
        open: &[
            r#"call selection-set {"start":7,"end":7}"#,
            r#"call ribbon-click {"tab":"Home","command":"sort"}"#,
        ],
        fields: &[
            dropdown("sort1"),
            dropdown("type1"),
            radio("order1"),
            dropdown("sort2"),
            dropdown("type2"),
            radio("order2"),
            dropdown("sort3"),
            dropdown("type3"),
            radio("order3"),
            radio("header"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    CONVERT_TO_TEXT = "convert-to-text" {
        surface: Surface::Doc,
        file: "doc-tables",
        fixture: TABLE_DOCX,
        open: &[
            r#"call selection-set {"start":7,"end":7}"#,
            r#"call ribbon-click {"tab":"Table Layout","command":"convert-to-text"}"#,
        ],
        fields: &[
            radio("sep"),
            Field::text("other")
                .sample("/")
                .prep(SEPARATE_WITH_OTHER)
                .applied(&[
                    "assert table is null",
                    "key home shift+end",
                    "key ctrl+c",
                    r#"call clipboard {"action":"read"}"#,
                    "assert reply.text is cell A/cell B",
                ]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    CONVERT_TEXT_TO_TABLE = "convert-text-to-table" {
        surface: Surface::Doc,
        file: "doc-tables",
        fixture: DOCX,
        open: &[
            "type one/two\tthree",
            "key home shift+end",
            r#"call ribbon-click {"tab":"Insert","command":"table"}"#,
            r#"call menu-click {"label":"Convert Text to Table..."}"#,
        ],
        // The text is "one/two<Tab>three": the dialog opens on 2 columns,
        // split at the Tab.
        fields: &[
            Field::number("cols", "3").applied(&[
                "assert table.columns is 3",
                "assert table.cells.0.1.0 is three",
            ]),
            radio("sep"),
            Field::text("other")
                .sample("/")
                .prep(SEPARATE_WITH_OTHER)
                .applied(&["assert table.cells.0.1.0 is two⇥three"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    // ---- Word: Design ------------------------------------------------------
    MORE_COLORS = "more-colors" {
        surface: Surface::Doc,
        file: "doc-design",
        fixture: DOCX,
        open: &[
            r#"call menu-open {"target":{"ribbon":["Design","Page Background","Page Color"]}}"#,
            r#"call menu-click {"label":"More Colors..."}"#,
        ],
        fields: &[
            Field::text("hex")
                .sample("C00000")
                .applied(&["assert status is Page color: Dark Red"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    FILL_EFFECTS = "fill-effects" {
        surface: Surface::Doc,
        file: "doc-design",
        fixture: DOCX,
        open: &[
            r#"call menu-open {"target":{"ribbon":["Design","Page Background","Page Color"]}}"#,
            r#"call menu-click {"label":"Fill Effects..."}"#,
        ],
        fields: &[
            radio("colors"),
            dropdown("color1"),
            dropdown("color2").prep(TWO_COLORS),
            radio("style"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    WATERMARK = "watermark" {
        surface: Surface::Doc,
        file: "doc-design",
        fixture: DOCX,
        open: &[
            r#"call menu-open {"target":{"ribbon":["Design","Page Background","Watermark"]}}"#,
            r#"call menu-click {"label":"Custom Watermark..."}"#,
        ],
        fields: &[
            radio("kind"),
            Field::text("text")
                .prep(TEXT_WATERMARK)
                .applied(&[r#"assert status is "Watermark: Ab1 ""#]),
            Field::text("font")
                .sample("Arial")
                .prep(TEXT_WATERMARK)
                .kept("Arial"),
            Field::text("size").sample("72").prep(TEXT_WATERMARK).kept("72"),
            dropdown("color").prep(TEXT_WATERMARK),
            checkbox("semi"),
            radio("layout"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    PAGE_BORDERS = "page-borders" {
        surface: Surface::Doc,
        file: "doc-design",
        fixture: DOCX,
        open: &["key alt g p b"],
        fields: &[
            radio("setting"),
            dropdown("style"),
            dropdown("color"),
            dropdown("width"),
            checkbox("top"),
            checkbox("left"),
            checkbox("bottom"),
            checkbox("right"),
            dropdown("apply"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    PAGE_BORDER_OPTIONS = "page-border-options" {
        surface: Surface::Doc,
        file: "doc-design",
        fixture: DOCX,
        open: &[
            "key alt g p b",
            r#"call dialog-click {"button":"Options..."}"#,
        ],
        fields: &[
            Field::number("top", "10").kept("10"),
            Field::number("left", "10").kept("10"),
            Field::number("bottom", "10").kept("10"),
            Field::number("right", "10").kept("10"),
            dropdown("from"),
        ],
        reopen: &[r#"call dialog-click {"button":"Options..."}"#],
        accept_closes: true,
        unreachable: None,
    }
    // ---- Word: Mailings ----------------------------------------------------
    MAIL_ENVELOPES = "mail-envelopes" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[r#"call ribbon-click {"tab":"Mailings","command":"Envelopes"}"#],
        fields: &[
            Field::text("delivery").applied(&[ENVELOPE_ADDED, "assert mail.text is Ab1"]),
            Field::text("return").applied(&[ENVELOPE_ADDED, "assert mail.text is Ab1"]),
            checkbox("omit"),
            dropdown("size"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_ENVELOPE_OPTIONS = "mail-envelope-options" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call menu-open {"target":{"ribbon":["Mailings","Start Mail Merge","Start Mail Merge"]}}"#,
            r#"call menu-click {"label":"Envelopes..."}"#,
        ],
        fields: &[
            dropdown("size"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_LABELS = "mail-labels" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[r#"call ribbon-click {"tab":"Mailings","command":"Labels"}"#],
        fields: &[
            // OK opens the label sheet in a new tab, a table of labels.
            Field::text("address").applied(&["assert table.cells.0.0.0 is Ab1"]),
            dropdown("product"),
            Field::number("width", "2").prep(CUSTOM_LABEL).no_ok(LABEL_SIZE),
            Field::number("height", "1.05").prep(CUSTOM_LABEL).no_ok(LABEL_SIZE),
            Field::number("across", "2")
                .prep(CUSTOM_LABEL)
                .applied(&["assert table.columns is 2"]),
            Field::number("down", "5")
                .prep(CUSTOM_LABEL)
                .applied(&["assert table.rows is 5"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_LABEL_OPTIONS = "mail-label-options" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call menu-open {"target":{"ribbon":["Mailings","Start Mail Merge","Start Mail Merge"]}}"#,
            r#"call menu-click {"label":"Labels..."}"#,
        ],
        fields: &[
            dropdown("product"),
            Field::number("width", "2").prep(CUSTOM_LABEL).no_ok(LABEL_SIZE),
            Field::number("height", "1.05").prep(CUSTOM_LABEL).no_ok(LABEL_SIZE),
            Field::number("across", "2")
                .prep(CUSTOM_LABEL)
                .applied(&["assert mail.doc_type is Labels", "assert table.columns is 2"]),
            Field::number("down", "5")
                .prep(CUSTOM_LABEL)
                .applied(&["assert mail.doc_type is Labels", "assert table.rows is 5"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_REPLACE = "mail-replace" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            "type Hello",
            r#"call menu-open {"target":{"ribbon":["Mailings","Start Mail Merge","Start Mail Merge"]}}"#,
            r#"call menu-click {"label":"Envelopes..."}"#,
            r#"call dialog-click {"button":"OK"}"#,
        ],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_RECIPIENTS = "mail-recipients" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"Edit Recipient List"}"#,
        ],
        fields: &[
            Field::number("record", "2")
                .no_ok("OK applies the grid's Include column; Record only moves the dialog's own view"),
            checkbox("include"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_ADDRESS_BLOCK = "mail-address-block" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"addressblock"}"#,
        ],
        fields: &[
            checkbox("company"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_GREETING_LINE = "mail-greeting-line" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"greetingline"}"#,
        ],
        fields: &[
            dropdown("salutation"),
            dropdown("name"),
            dropdown("punctuation"),
            Field::text("fallback").no_ok(
                "it shows only for a record without a name, and recipients.csv names everyone",
            ),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_MATCH_FIELDS = "mail-match-fields" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"matchfields"}"#,
        ],
        fields: &[
            dropdown("title"),
            dropdown("first"),
            dropdown("middle"),
            dropdown("last"),
            dropdown("suffix"),
            dropdown("nickname"),
            dropdown("company"),
            dropdown("address1"),
            dropdown("address2"),
            dropdown("city"),
            dropdown("state"),
            dropdown("postal"),
            dropdown("country"),
            dropdown("email"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_FIND = "mail-find" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"findrecipient"}"#,
        ],
        fields: &[
            Field::text("find")
                .sample("John")
                .applied(&["assert status is Found in record 2", "assert mail.record is 2"]),
            dropdown("field"),
        ],
        reopen: &[],
        accept_closes: false,
        unreachable: None,
    }
    MAIL_CHECK_ERRORS = "mail-check-errors" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"checkerrors"}"#,
        ],
        fields: &[
            radio("mode"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_REPORT = "mail-report" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"checkerrors"}"#,
            r#"call dialog-click {"button":"OK"}"#,
        ],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_MERGE_NEW = "mail-merge-new" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[
            "key ctrl+a",
            "type Dear",
            r#"call mail-attach {"path":"recipients.csv"}"#,
            r#"call menu-open {"target":{"ribbon":["Mailings","Write & Insert Fields","Insert Merge Field"]}}"#,
            r#"call menu-click {"label":"First Name"}"#,
            r#"call ribbon-click {"tab":"Mailings","command":"Edit Individual Documents..."}"#,
        ],
        fields: &[
            radio("records"),
            Field::number("from", "2")
                .prep(RECORDS_FROM)
                .applied(&["assert mail.text is DearJohn¶DearAmy"]),
            Field::number("to", "2")
                .prep(RECORDS_FROM)
                .applied(&["assert mail.text is DearJane¶DearJohn"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    MAIL_ATTACH = "mail-attach" {
        surface: Surface::Doc,
        file: "doc-mailings",
        fixture: DOCX,
        open: &[],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: Some(
            "it needs a document whose mail merge names an absolute data source not yet read; no fixture has one",
        ),
    }
    // ---- App dialogs, from a document tab ----------------------------------
    SAVE_ON_CLOSE = "save-on-close" {
        surface: Surface::Doc,
        file: "doc-app",
        fixture: "copy:../fixtures/basic.docx",
        open: &["type x", "key ctrl+w"],
        fields: &[
            Field::text("file-name").no_ok(
                "Save writes into the case's sandbox folder, which a script cannot name; tab-close.uit covers the save",
            ),
            dropdown("location"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    REOPEN = "reopen" {
        surface: Surface::Doc,
        file: "doc-app",
        fixture: "copy:../fixtures/basic.docx",
        open: &["type x", r#"call open {"path":"basic.docx","reopen":"ask"}"#],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    USER_NAME = "user-name" {
        surface: Surface::Doc,
        file: "doc-app",
        fixture: DOCX,
        // From Backstage, where #1027 hid: a real click on its Settings row.
        open: &[
            r#"call backstage {"action":"open"}"#,
            r#"call pointer-wheel {"region":"backstage-content","dy":-6000}"#,
            "shot window",
            r#"call pointer-click {"at":"user-name-row"}"#,
        ],
        fields: &[
            Field::text("user-name").applied(&["assert user_name is Ab1"]),
            Field::text("initials").applied(&["assert user_initials is Ab1"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    ABOUT = "about" {
        surface: Surface::Doc,
        file: "doc-app",
        fixture: DOCX,
        open: &[r#"call ribbon-click {"tab":"Help","command":"About docxy suite"}"#],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    AUTOCORRECT = "autocorrect" {
        surface: Surface::Doc,
        file: "doc-app",
        fixture: DOCX,
        open: &["call autocorrect {}"],
        fields: &[
            checkbox("ac_show_buttons").on("AutoCorrect"),
            checkbox("ac_two_initial_caps").on("AutoCorrect"),
            checkbox("ac_first_letter").on("AutoCorrect"),
            checkbox("ac_names_of_days").on("AutoCorrect"),
            checkbox("ac_caps_lock").on("AutoCorrect"),
            checkbox("ac_replace_text").on("AutoCorrect"),
            checkbox("ac_hyperlinks").on("AutoFormat As You Type"),
            checkbox("ac_table_rows_cols").on("AutoFormat As You Type"),
            checkbox("ac_table_formulas").on("AutoFormat As You Type"),
            checkbox("ac_additional_actions").on("Actions"),
            checkbox("ac_math_outside").on("Math AutoCorrect"),
            checkbox("ac_math_replace").on("Math AutoCorrect"),
            Field::text("replace").on("AutoCorrect").no_ok(THROUGH_ADD),
            Field::text("with").on("AutoCorrect").no_ok(THROUGH_ADD),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    AUTOCORRECT_EXCEPTIONS = "autocorrect-exceptions" {
        surface: Surface::Doc,
        file: "doc-app",
        fixture: DOCX,
        open: &[
            "call autocorrect {}",
            r#"call dialog-click {"button":"Exceptions..."}"#,
        ],
        fields: &[
            Field::text("first-word").on("First Letter").no_ok(THROUGH_ADD),
            Field::text("caps-word").on("INitial CAps").no_ok(THROUGH_ADD),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    AUTOCORRECT_REDEFINE = "autocorrect-redefine" {
        surface: Surface::Doc,
        file: "doc-app",
        fixture: DOCX,
        open: &[
            "call autocorrect {}",
            r#"call dialog-set {"control":"replace","value":"zq"}"#,
            r#"call dialog-set {"control":"with","value":"one"}"#,
            r#"call dialog-click {"button":"Add"}"#,
            r#"call dialog-set {"control":"replace","value":"zq"}"#,
            r#"call dialog-set {"control":"with","value":"two"}"#,
            r#"call dialog-click {"button":"Add"}"#,
        ],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    // ---- Excel -------------------------------------------------------------
    SHEET_PAGE_SETUP = "page-setup" {
        surface: Surface::Sheet,
        file: "sheet-layout",
        fixture: XLSX,
        open: &[r#"call ribbon-click {"tab":"Page Layout","command":"Print Titles"}"#],
        fields: &[
            radio("orientation").on("Page"),
            radio("scaling").on("Page"),
            Field::number("scale", "80").on("Page").kept("80"),
            Field::number("fit-width", "2").on("Page").kept("2"),
            Field::number("fit-height", "2").on("Page").kept("2"),
            dropdown("paper").on("Page"),
            Field::number("top", "1.5").on("Margins").kept("1.5"),
            Field::number("bottom", "1.5").on("Margins").kept("1.5"),
            Field::number("left", "1.5").on("Margins").kept("1.5"),
            Field::number("right", "1.5").on("Margins").kept("1.5"),
            Field::number("header", "0.4").on("Margins").kept("0.4"),
            Field::number("footer", "0.4").on("Margins").kept("0.4"),
            checkbox("h-centered").on("Margins"),
            checkbox("v-centered").on("Margins"),
            Field::text("print-area").on("Sheet").sample("A1:C5").kept("A1:C5"),
            Field::text("title-rows").on("Sheet").sample("$1:$1").kept("$1:$1"),
            Field::text("title-cols").on("Sheet").sample("$A:$A").kept("$A:$A"),
            checkbox("gridlines").on("Sheet"),
            checkbox("headings").on("Sheet"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    CONSOLIDATE = "consolidate" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["click E1", r#"call ribbon-click {"tab":"Data","command":"Consolidate"}"#],
        fields: &[
            dropdown("function"),
            Field::text("reference")
                .sample("B2:B5")
                .applied(&["assert cell E1 is 10", "assert cell E4 is 40"]),
            checkbox("top"),
            checkbox("left"),
            checkbox("links"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    DROP_REPLACE = "drop-replace" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[r#"call border-drag {"from":"B2","to":"A2"}"#],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    SERIES = "series" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[
            "click Q1",
            "type 1",
            "key enter",
            "click Q1",
            "click Q6 shift",
            r#"call ribbon-click {"tab":"Home","command":"Fill"}"#,
            r#"call menu-click {"label":"Series…"}"#,
        ],
        fields: &[
            radio("series-in"),
            radio("type"),
            radio("unit"),
            checkbox("trend"),
            Field::text("step").sample("2").applied(&["assert cell Q2 is 3", "assert cell Q3 is 5"]),
            Field::text("stop").sample("3").applied(&["assert cell Q3 is 3", "assert cell Q4 is nothing"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    JUSTIFY = "justify" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[
            "click P10",
            "type alpha beta gamma",
            "key enter",
            "click P10",
            r#"call menu-open {"target":{"ribbon":["Home","Editing","Fill"]}}"#,
            r#"call menu-click {"label":"Justify"}"#,
        ],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    CUSTOM_LISTS = "custom-lists" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[
            r#"call backstage {"action":"open"}"#,
            r#"call pointer-wheel {"region":"backstage-content","dy":-6000}"#,
            "shot window",
            r#"call pointer-click {"at":"custom-lists-row"}"#,
        ],
        fields: &[
            Field::text("entries").sample("Lo, Mid, Hi").no_ok(THROUGH_ADD),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    FLASH_FILL = "flash-fill" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["click E1", "type zzz", "key enter", "click E2", "key ctrl+e"],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    GOTO = "goto" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["key ctrl+g"],
        fields: &[
            Field::text("reference").sample("K3:L4").applied(&["assert range is K3:L4"]),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    GOTO_SPECIAL = "goto-special" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["key f5", r#"call dialog-click {"button":"Special…"}"#],
        fields: &[
            radio("select"),
            checkbox("numbers"),
            checkbox("text"),
            checkbox("logicals"),
            checkbox("errors"),
            radio("levels"),
            radio("rules"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    GROUP = "group" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["click A2", "click B3 shift", r#"call ribbon-click {"tab":"Data","command":"Group"}"#],
        fields: &[
            radio("axis"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    UNGROUP = "ungroup" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["click A2", "click B3 shift", r#"call ribbon-click {"tab":"Data","command":"Ungroup"}"#],
        fields: &[
            radio("axis"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    SUBTOTAL = "subtotal" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["click A2", r#"call ribbon-click {"tab":"Data","command":"Subtotal"}"#],
        fields: &[
            dropdown("group"),
            dropdown("function"),
            checkbox("add-0"),
            checkbox("add-1"),
            checkbox("add-2"),
            checkbox("replace"),
            checkbox("page-breaks"),
            checkbox("below"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    OUTLINE_SETTINGS = "outline-settings" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[r#"call ribbon-click {"tab":"Data","command":"Settings..."}"#],
        fields: &[
            checkbox("below"),
            checkbox("right"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    PASTE_SPECIAL = "paste-special" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["click B2", "key ctrl+c", "click E2", "key ctrl+alt+v"],
        fields: &[
            radio("paste"),
            radio("operation"),
            checkbox("skip-blanks"),
            checkbox("transpose"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    DATA_VALIDATION = "data-validation" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &["click B2", r#"call ribbon-click {"tab":"Data","command":"Data Validation"}"#],
        fields: &[
            dropdown("allow").on("Settings"),
            dropdown("operator").on("Settings").prep(WHOLE_NUMBER),
            Field::text("first")
                .on("Settings")
                .sample("10")
                .prep(WHOLE_NUMBER_MAX)
                .applied(&[VALIDATION_APPLIED])
                .kept("10"),
            Field::text("second")
                .on("Settings")
                .sample("90")
                .prep(WHOLE_NUMBER_MIN)
                .applied(&[VALIDATION_APPLIED])
                .kept("90"),
            checkbox("ignore-blank").on("Settings"),
            checkbox("dropdown").on("Settings"),
            checkbox("apply-all").on("Settings"),
            checkbox("show-input").on("Input Message"),
            Field::text("input-title").on("Input Message").applied(&[VALIDATION_APPLIED]).kept("Ab1 "),
            Field::text("input-message").on("Input Message").applied(&[VALIDATION_APPLIED]).kept("Ab1 "),
            checkbox("show-error").on("Error Alert"),
            dropdown("error-style").on("Error Alert"),
            Field::text("error-title").on("Error Alert").applied(&[VALIDATION_APPLIED]).kept("Ab1 "),
            Field::text("error-message").on("Error Alert").applied(&[VALIDATION_APPLIED]).kept("Ab1 "),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    DATA_VALIDATION_ALERT = "data-validation-alert" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[
            "click B2",
            r#"call ribbon-click {"tab":"Data","command":"Data Validation"}"#,
            r#"call dialog-set {"control":"allow","value":"Whole number"}"#,
            r#"call dialog-set {"control":"first","value":"10"}"#,
            r#"call dialog-set {"control":"second","value":"90"}"#,
            r#"call dialog-click {"button":"OK"}"#,
            "click B2",
            "type 5",
            "key enter",
        ],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    TEXT_TO_COLUMNS = "text-to-columns" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[
            "click J1",
            "type a,b",
            "key enter",
            "click J1",
            r#"call ribbon-click {"tab":"Data","command":"Text to Columns"}"#,
        ],
        fields: &[
            radio("kind").on("Step 1: Data type"),
            checkbox("tab").on(STEP2),
            checkbox("semicolon").on(STEP2),
            checkbox("comma").on(STEP2),
            checkbox("space").on(STEP2),
            checkbox("other").on(STEP2),
            Field::text("other_char")
                .on(STEP2)
                .sample("|")
                .no_ok("its character splits only with Other checked; a, b has none"),
            checkbox("consecutive").on(STEP2),
            dropdown("qualifier").on(STEP2),
            Field::text("breaks")
                .on(STEP2)
                .sample("1")
                .prep(FIXED_WIDTH)
                .applied(&["assert cell J1 is a", "assert cell K1 is ,b"]),
            dropdown("column").on(STEP3).prep(TWO_COLUMNS),
            radio("format").on(STEP3),
            dropdown("date_order").on(STEP3),
            Field::text("formats")
                .on(STEP3)
                .sample("general, skip")
                .prep(TWO_COLUMNS)
                .applied(&["assert cell J1 is a", "assert cell K1 is nothing"]),
            Field::text("destination")
                .on(STEP3)
                .sample("L1")
                .applied(&["assert cell L1 is a,b"]),
            Field::text("decimal").on(STEP3).sample(";").no_ok(SEPARATORS),
            Field::text("thousands").on(STEP3).sample("'").no_ok(SEPARATORS),
            checkbox("trailing_minus").on(STEP3),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    TEXT_TO_COLUMNS_REPLACE = "text-to-columns-replace" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: XLSX,
        open: &[
            "click K1",
            "type x",
            "key enter",
            "click J1",
            "type a,b",
            "key enter",
            "click J1",
            r#"call ribbon-click {"tab":"Data","command":"Text to Columns"}"#,
            r#"call dialog-tab {"tab":"Step 2: Delimiters"}"#,
            r#"call dialog-set {"control":"comma","value":true}"#,
            r#"call dialog-click {"button":"Finish"}"#,
        ],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    FILTER_MENU = "filter-menu" {
        surface: Surface::Sheet,
        file: "sheet-sort-filter",
        fixture: XLSX,
        open: &[
            "click A2",
            "key ctrl+shift+l",
            "shot filter-button:A",
            r#"call pointer-click {"region":"filter-button:A"}"#,
        ],
        fields: &[
            dropdown("color").skip("enabled only when the column has colours; basic.xlsx has none"),
            dropdown("typed"),
            Field::text("search").sample("Ea").no_ok("the search takes effect through its Search button"),
            checkbox("add"),
            Field::other("values", ControlKind::CheckList),
            Field::text("searched").skip("hidden: the query the value list was last searched with"),
        ],
        reopen: &[
            "shot filter-button:A",
            r#"call pointer-click {"region":"filter-button:A"}"#,
        ],
        accept_closes: true,
        unreachable: None,
    }
    CUSTOM_AUTOFILTER = "custom-autofilter" {
        surface: Surface::Sheet,
        file: "sheet-sort-filter",
        fixture: XLSX,
        open: &[
            "click A2",
            "key ctrl+shift+l",
            "shot filter-button:B",
            r#"call pointer-click {"region":"filter-button:B"}"#,
            r#"call dialog-set {"control":"typed","value":"Greater Than..."}"#,
            r#"call dialog-click {"button":"Apply Filter"}"#,
        ],
        fields: &[
            dropdown("op1"),
            Field::text("val1").sample("20").applied(&["assert status is 2 of 4 records found"]),
            radio("join"),
            dropdown("op2"),
            Field::text("val2").sample("35").no_ok("its value is read only with a second operator chosen"),
        ],
        reopen: &[
            "shot filter-button:B",
            r#"call pointer-click {"region":"filter-button:B"}"#,
            r#"call dialog-set {"control":"typed","value":"Greater Than..."}"#,
            r#"call dialog-click {"button":"Apply Filter"}"#,
        ],
        accept_closes: true,
        unreachable: None,
    }
    TOP10_AUTOFILTER = "top10-autofilter" {
        surface: Surface::Sheet,
        file: "sheet-sort-filter",
        fixture: XLSX,
        open: &[
            "click A2",
            "key ctrl+shift+l",
            "shot filter-button:B",
            r#"call pointer-click {"region":"filter-button:B"}"#,
            r#"call dialog-set {"control":"typed","value":"Top 10..."}"#,
            r#"call dialog-click {"button":"Apply Filter"}"#,
        ],
        fields: &[
            dropdown("which"),
            Field::number("n", "1").applied(&["assert status is 1 of 4 records found"]),
            dropdown("unit"),
        ],
        reopen: &[
            "shot filter-button:B",
            r#"call pointer-click {"region":"filter-button:B"}"#,
            r#"call dialog-set {"control":"typed","value":"Top 10..."}"#,
            r#"call dialog-click {"button":"Apply Filter"}"#,
        ],
        accept_closes: true,
        unreachable: None,
    }
    ADVANCED_FILTER = "advanced-filter" {
        surface: Surface::Sheet,
        file: "sheet-sort-filter",
        fixture: XLSX,
        open: &[
            "click E1",
            "type Region",
            "key enter",
            "type East",
            "key enter",
            "click A1",
            r#"call ribbon-click {"tab":"Data","command":"Advanced"}"#,
        ],
        fields: &[
            radio("action"),
            // North and South only: no East record for the criteria.
            Field::text("list")
                .sample("A1:C3")
                .prep(CRITERIA)
                .applied(&["assert status is 0 of 2 records found"]),
            Field::text("criteria")
                .sample("E1:E2")
                .applied(&["assert status is 1 of 4 records found"]),
            Field::text("copy")
                .sample("G1")
                .no_ok("Copy to is read only with Copy to another location chosen"),
            checkbox("unique"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    SHEET_SORT = "sort" {
        surface: Surface::Sheet,
        file: "sheet-sort-filter",
        fixture: XLSX,
        open: &["click A2", r#"call ribbon-click {"tab":"Data","command":"Sort"}"#],
        fields: &[
            dropdown("by1"),
            dropdown("on1"),
            dropdown("order1"),
            Field::text("with1").sample("West, East").no_ok(SORT_LIST),
            dropdown("by2"),
            dropdown("on2"),
            dropdown("order2"),
            Field::text("with2").sample("West, East").no_ok(SORT_LIST),
            dropdown("by3"),
            dropdown("on3"),
            dropdown("order3"),
            Field::text("with3").sample("West, East").no_ok(SORT_LIST),
            checkbox("headers"),
            checkbox("case"),
            radio("orientation"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    SORT_WARNING = "sort-warning" {
        surface: Surface::Sheet,
        file: "sheet-sort-filter",
        fixture: XLSX,
        open: &[
            "click B2",
            "click B5 shift",
            r#"call ribbon-click {"tab":"Data","command":"Sort Z to A"}"#,
        ],
        fields: &[
            radio("what"),
        ],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    SHEET_SAVE_ON_CLOSE = "save-on-close" {
        surface: Surface::Sheet,
        file: "sheet-data",
        fixture: "copy:../fixtures/basic.xlsx",
        open: &["click B2", "type 5", "key enter", "call close-tab {}"],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    // ---- Project -----------------------------------------------------------
    DELETE_SUMMARY = "delete-summary" {
        surface: Surface::Project,
        file: "project",
        fixture: "../fixtures/gantt-summary.xml",
        open: &["click A1", "key delete"],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
    PROJECT_SAVE_ON_CLOSE = "save-on-close" {
        surface: Surface::Project,
        file: "project",
        fixture: "copy:../fixtures/gantt-summary.xml",
        open: &["click A1", "key delete", "key enter", "call close-tab {}"],
        fields: &[],
        reopen: &[],
        accept_closes: true,
        unreachable: None,
    }
}
