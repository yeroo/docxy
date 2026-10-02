//! Import of the word-processing formats that are not WordprocessingML
//! (#634): the Word 97-2003 binary document (`.doc`, MS-DOC in an OLE2
//! compound file).
//!
//! Opening one is an *import*: the reader builds a fresh [`Package`] (as
//! [`new_package`] makes one), so editor, render and save see an ordinary
//! document and saving writes `.docx`. Nothing the reader doesn't model
//! survives the import.
//!
//! [`Package`]: crate::package::Package
//! [`new_package`]: crate::package::new_package

pub mod doc;
