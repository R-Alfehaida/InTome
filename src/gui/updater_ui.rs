//! STUB: the UI half of prism/updater.py over `crate::updater`. The signature is the contract.

use winsafe::prelude::*;

/// Checks for updates on a worker thread, then prompts, downloads with a
/// progress dialog, verifies, and launches the installer. `silent` suppresses
/// the "already up to date" and network-error messages.
pub fn check_for_updates(_parent: &impl GuiParent, _silent: bool) {}
