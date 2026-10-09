//! STUB: port of prism/gui/form_dialog.py. The signature is the contract.

use winsafe::prelude::*;

use crate::document::FormField;
use crate::document::formats::pdf::PdfDocument;

/// The accessible form-filling dialog for `fields` of an open PDF, including
/// Save (to the original) and Save As. Emits `FormSaved` after a save.
pub fn show(_parent: &impl GuiParent, _doc: &mut PdfDocument, _fields: Vec<FormField>, _filename: &str) {}
