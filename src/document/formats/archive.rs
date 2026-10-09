//! STUB: replaced by the port of the matching Prism module.
use std::path::Path;
use crate::document::{Document, OpenError, OpenOptions};
pub fn open_zip(_path: &Path, _opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> { Err(OpenError::Unavailable("not yet ported".into())) }
pub fn open_rar(_path: &Path, _opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> { Err(OpenError::Unavailable("not yet ported".into())) }
