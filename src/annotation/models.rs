//! Annotation rows. Table and column names match Prism's SQLAlchemy models, so an existing
//! `annotations.db` opens unchanged. Annotations are keyed by `document_hash`
//! (`document::cache::file_hash`).

use chrono::NaiveDateTime;

#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    pub id: i64,
    pub document_hash: String,
    pub name: String,
    pub page_index: usize,
    pub char_position: usize,
    pub created_at: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: i64,
    pub document_hash: String,
    pub page_index: usize,
    pub char_position: usize,
    pub text: String,
    pub created_at: Option<NaiveDateTime>,
    pub updated_at: Option<NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub id: i64,
    pub document_hash: String,
    pub page_index: usize,
    pub start_pos: usize,
    pub end_pos: usize,
    pub text: String,
    pub tags: String,
    pub created_at: Option<NaiveDateTime>,
}

/// SQLAlchemy's SQLite DateTime text format (naive UTC): `2026-10-09 02:16:25.123456`.
pub(crate) const DB_TIME_FORMAT: &str = "%Y-%m-%d %H:%M:%S%.6f";

pub(crate) fn now_db() -> String {
    chrono::Utc::now().format(DB_TIME_FORMAT).to_string()
}

pub(crate) fn parse_db_time(s: Option<String>) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(&s?, "%Y-%m-%d %H:%M:%S%.f").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_time_round_trips_sqlalchemy_format() {
        let t = parse_db_time(Some("2026-10-09 02:16:25.123456".into())).unwrap();
        assert_eq!(t.format(DB_TIME_FORMAT).to_string(), "2026-10-09 02:16:25.123456");
        assert!(parse_db_time(Some("2026-10-09 02:16:25".into())).is_some());
        assert!(parse_db_time(None).is_none());
    }
}
