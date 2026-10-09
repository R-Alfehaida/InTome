//! STUB: replaced by the port of the matching Prism module.
use std::path::Path;
use crate::document::{Document, OpenError, OpenOptions};
pub fn open(_path: &Path, _opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> { Err(OpenError::Unavailable("not yet ported".into())) }
pub fn open_doc(_path: &Path, _opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> { Err(OpenError::Unavailable("not yet ported".into())) }
pub fn doc_available() -> bool { false }
