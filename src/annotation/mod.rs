//! Bookmarks, notes and quotes in a per-user SQLite database (port of `prism/annotation`).

pub mod exporters;
pub mod manager;
pub mod models;

pub use manager::{AnnotationCounts, AnnotationManager};
pub use models::{Bookmark, Note, Quote};
