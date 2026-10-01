//! Placeholder until the reader lands.

use super::{BookIn, OpenError};

pub(crate) fn read(_zip: &opccore::zip::ZipArchive) -> Result<BookIn, OpenError> {
    Err(OpenError::Corrupt(
        "OpenDocument files are not read yet".into(),
    ))
}
