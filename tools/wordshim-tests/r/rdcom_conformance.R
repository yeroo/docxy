# RDCOMClient conformance test for the Word shim (wordcomshim).
#
# Drives Word over COM from R via RDCOMClient's own C++ IDispatch client. Builds a
# document with formatting and validates the produced .docx directly (no Word).
#
# Run (shim installed + typelib registered):
#   Rscript rdcom_conformance.R
# Exit 0 = PASS. Skips cleanly if RDCOMClient isn't installed.

.libPaths(c(file.path(Sys.getenv("USERPROFILE"), "R", "win-library",
                      paste(R.version$major, sub("\\..*", "", R.version$minor), sep = ".")),
            .libPaths()))

if (!requireNamespace("RDCOMClient", quietly = TRUE)) {
  cat("SKIP: RDCOMClient not installed\n"); quit(status = 0)
}
library(RDCOMClient)

docx <- file.path(Sys.getenv("TEMP"), "rdcom-wd-conformance.docx")
if (file.exists(docx)) file.remove(docx)

wd <- COMCreate("Word.Application")
wd[["Visible"]] <- FALSE
docs <- wd[["Documents"]]; doc <- docs$Add()
sel <- wd[["Selection"]]
sf <- sel[["Font"]]; sf[["Bold"]] <- TRUE
sel$TypeText("RDComWordTitle"); sel$TypeParagraph()
sf[["Bold"]] <- FALSE; sf[["Size"]] <- 14
sel$TypeText("RDComWordBody"); sel$TypeParagraph()
pf <- sel[["ParagraphFormat"]]; pf[["Alignment"]] <- 1   # center (before the mark)
sel$TypeText("RDComWordCentered"); sel$TypeParagraph()
doc$SaveAs2(docx); doc$Close(); wd$Quit()

fail <- character(0)
if (!file.exists(docx)) {
  fail <- "no file produced"
} else {
  files <- unzip(docx, list = TRUE)$Name
  if (!("word/document.xml" %in% files)) fail <- c(fail, "missing word/document.xml")
  if ("docProps/app.xml" %in% files) fail <- c(fail, "docProps/app.xml -> real Word served")
  con <- unz(docx, "word/document.xml"); x <- paste(readLines(con, warn = FALSE), collapse = "\n"); close(con)
  checks <- list("bold title" = "RDComWordTitle", "sized body" = "RDComWordBody",
                 "centered" = "RDComWordCentered", "bold run" = "<w:b",
                 "font size" = "<w:sz", "center align" = 'w:val="center"')
  for (nm in names(checks)) if (!grepl(checks[[nm]], x, fixed = TRUE)) fail <- c(fail, paste("missing", nm))
}

sz <- if (file.exists(docx)) file.info(docx)$size else 0
if (length(fail) == 0) {
  cat(sprintf("  PASS [R %s]: created %s (%d bytes)\n", getRversion(), docx, sz))
  cat("RDCOMCLIENT WORD CONFORMANCE: PASS\n"); quit(status = 0)
} else {
  cat(sprintf("  FAIL [R %s]: %s\n", getRversion(), paste(fail, collapse = ", ")))
  cat("RDCOMCLIENT WORD CONFORMANCE: FAIL\n"); quit(status = 1)
}
