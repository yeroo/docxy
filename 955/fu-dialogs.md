Deferred from #955 (itself the leftovers of #940 / PR #954), item **plan-dialogs**.

#940 adds a terminal Design tab with fixed pick lists only (20 palette colours, Word's 12 watermark presets, page borders None/Box/Shadow). The terminal still lacks the dialogs the suite has: Custom Watermark (text, font, size, colour, layout), Page Color's More Colors and Fill Effects, and the full Borders and Shading Page Border tab (style, width, colour, apply-to, art).

#955 adds control/MCP verbs (`doc.page-color`, `doc.watermark`, `doc.page-borders`) that already accept an arbitrary colour, watermark text/font/colour and border colour, so the dialogs can build on the App methods `set_page_color`, `set_text_watermark`, `set_page_borders` introduced there.

Oracle: suite/docxy/src/design_dialogs.rs.
