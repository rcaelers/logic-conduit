use zip::result::ZipError;

use signal_processing::Error;

pub(crate) fn zip_error(error: ZipError) -> Error {
    Error::ParseError(format!("capture archive error: {error}"))
}
