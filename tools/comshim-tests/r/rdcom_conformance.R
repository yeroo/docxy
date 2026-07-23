# RDCOMClient conformance test for the Excel shim (xlcomshim).
#
# RDCOMClient drives Office over COM from R via its own C++ IDispatch client — an
# implementation independent of VBScript, pywin32, and the CLR. Builds a workbook
# and validates the produced .xlsx directly (no Excel needed to open it).
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

validate <- function(path, part, needle) {
  if (!file.exists(path)) return("no file produced")
  files <- unzip(path, list = TRUE)$Name
  if (!(part %in% files)) return(paste("missing", part))
  # gridcore never writes docProps/app.xml; real Excel always does.
  if ("docProps/app.xml" %in% files) return("docProps/app.xml -> real Excel served")
  for (f in files[endsWith(files, ".xml")]) {
    con <- unz(path, f); t <- paste(readLines(con, warn = FALSE), collapse = "\n"); close(con)
    if (grepl(needle, t, fixed = TRUE)) return("ok")
  }
  paste("text", needle, "not found")
}

xlsx <- file.path(Sys.getenv("TEMP"), "rdcom-xl-conformance.xlsx")
if (file.exists(xlsx)) file.remove(xlsx)

xl <- COMCreate("Excel.Application")
xl[["Visible"]] <- FALSE
wbs <- xl[["Workbooks"]]; wb <- wbs$Add()
wss <- wb[["Worksheets"]]; ws <- wss$Item(1)
a1 <- ws$Range("A1"); a1[["Value2"]] <- "RDComConformance"
c2 <- ws$Cells(2, 1); c2[["Value2"]] <- 10
c3 <- ws$Cells(3, 1); c3[["Value2"]] <- 32
a4 <- ws$Range("A4"); a4[["Formula"]] <- "=SUM(A2:A3)"
fnt <- a1[["Font"]]; fnt[["Bold"]] <- TRUE
wb$SaveAs(xlsx, 51); wb$Close(FALSE); xl$Quit()

r <- validate(xlsx, "xl/workbook.xml", "RDComConformance")
sz <- if (file.exists(xlsx)) file.info(xlsx)$size else 0
cat(sprintf("  %s [R %s]: %s (%d bytes)\n",
            if (r == "ok") "PASS" else "FAIL", getRversion(), r, sz))
cat("RDCOMCLIENT EXCEL CONFORMANCE:", if (r == "ok") "PASS\n" else "FAIL\n")
quit(status = if (r == "ok") 0 else 1)
