//! GUI-independent navigation over a [`Document`](crate::document::Document):
//! semantic elements across pages (`element_nav`), the reading position and
//! what each move speaks (`reading_position`), and full-text search with
//! match tracking (`search`). Ports of prism/navigation.

pub mod element_nav;
pub mod reading_position;
pub mod search;
