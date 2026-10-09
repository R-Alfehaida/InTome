//! CRUD for bookmarks, notes and quotes.
//!
//! Like Prism's session-per-call design, every method opens its own SQLite connection, so the
//! manager is `Send + Sync` and can be shared between the UI and worker threads. Mutating
//! methods emit the matching [`AppEvent`] and, when `annotations.speak_bookmark_confirmation`
//! is on, announce for screen readers.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Context;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::annotation::exporters::{html::export_annotations_html, text::export_annotations_text};
use crate::annotation::models::{Bookmark, Note, Quote, now_db, parse_db_time};
use crate::config;
use crate::constants;
use crate::events::{self, AppEvent};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS bookmarks (
    id INTEGER NOT NULL,
    document_hash VARCHAR(32) NOT NULL,
    name VARCHAR(200),
    page_index INTEGER NOT NULL,
    char_position INTEGER,
    created_at DATETIME,
    PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS ix_bookmarks_document_hash ON bookmarks (document_hash);
CREATE TABLE IF NOT EXISTS notes (
    id INTEGER NOT NULL,
    document_hash VARCHAR(32) NOT NULL,
    page_index INTEGER NOT NULL,
    char_position INTEGER,
    text TEXT NOT NULL,
    created_at DATETIME,
    updated_at DATETIME,
    PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS ix_notes_document_hash ON notes (document_hash);
CREATE TABLE IF NOT EXISTS quotes (
    id INTEGER NOT NULL,
    document_hash VARCHAR(32) NOT NULL,
    page_index INTEGER NOT NULL,
    start_pos INTEGER NOT NULL,
    end_pos INTEGER NOT NULL,
    text TEXT NOT NULL,
    tags VARCHAR(500),
    created_at DATETIME,
    PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS ix_quotes_document_hash ON quotes (document_hash);
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnnotationCounts {
    pub bookmarks: usize,
    pub notes: usize,
    pub quotes: usize,
}

impl AnnotationCounts {
    pub fn total(&self) -> usize {
        self.bookmarks + self.notes + self.quotes
    }
}

pub struct AnnotationManager {
    path: PathBuf,
    ready: AtomicBool,
}

/// SQLite integers are i64; positions are never negative.
fn uz(r: &Row, i: usize) -> rusqlite::Result<usize> {
    Ok(r.get::<_, i64>(i)?.max(0) as usize)
}

fn uz_or0(r: &Row, i: usize) -> rusqlite::Result<usize> {
    Ok(r.get::<_, Option<i64>>(i)?.unwrap_or(0).max(0) as usize)
}

fn bookmark_from(r: &Row) -> rusqlite::Result<Bookmark> {
    Ok(Bookmark {
        id: r.get(0)?,
        document_hash: r.get(1)?,
        name: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        page_index: uz(r, 3)?,
        char_position: uz_or0(r, 4)?,
        created_at: parse_db_time(r.get(5)?),
    })
}

fn note_from(r: &Row) -> rusqlite::Result<Note> {
    Ok(Note {
        id: r.get(0)?,
        document_hash: r.get(1)?,
        page_index: uz(r, 2)?,
        char_position: uz_or0(r, 3)?,
        text: r.get(4)?,
        created_at: parse_db_time(r.get(5)?),
        updated_at: parse_db_time(r.get(6)?),
    })
}

fn quote_from(r: &Row) -> rusqlite::Result<Quote> {
    Ok(Quote {
        id: r.get(0)?,
        document_hash: r.get(1)?,
        page_index: uz(r, 2)?,
        start_pos: uz(r, 3)?,
        end_pos: uz(r, 4)?,
        text: r.get(5)?,
        tags: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
        created_at: parse_db_time(r.get(7)?),
    })
}

fn confirm(message: impl Into<String>) {
    if config::get().annotations.speak_bookmark_confirmation {
        events::announce(message);
    }
}

impl AnnotationManager {
    /// A manager over the database at `path` (created on first use).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), ready: AtomicBool::new(false) }
    }

    /// The shared manager over `constants::annotations_db()`.
    pub fn global() -> &'static AnnotationManager {
        static GLOBAL: OnceLock<AnnotationManager> = OnceLock::new();
        GLOBAL.get_or_init(|| AnnotationManager::new(constants::annotations_db()))
    }

    fn conn(&self) -> anyhow::Result<Connection> {
        if !self.ready.load(Ordering::Acquire)
            && let Some(dir) = self.path.parent()
        {
            std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let conn = Connection::open(&self.path).with_context(|| format!("cannot open {}", self.path.display()))?;
        if !self.ready.load(Ordering::Acquire) {
            conn.execute_batch(SCHEMA).context("cannot create annotation tables")?;
            self.ready.store(true, Ordering::Release);
        }
        Ok(conn)
    }

    // Bookmarks

    pub fn add_bookmark(&self, document_hash: &str, page_index: usize, char_position: usize, name: &str) -> anyhow::Result<Bookmark> {
        let conn = self.conn()?;
        let now = now_db();
        conn.execute(
            "INSERT INTO bookmarks (document_hash, name, page_index, char_position, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![document_hash, name, page_index as i64, char_position as i64, now],
        )?;
        let id = conn.last_insert_rowid();
        let bookmark = conn.query_row(
            "SELECT id, document_hash, name, page_index, char_position, created_at FROM bookmarks WHERE id = ?1",
            [id],
            bookmark_from,
        )?;
        events::emit(AppEvent::BookmarkAdded { document_hash: document_hash.into(), page_index, name: name.into() });
        let label = if name.is_empty() { format!("page {}", page_index + 1) } else { name.to_string() };
        confirm(format!("Bookmark added: {label}"));
        Ok(bookmark)
    }

    /// No-op if the bookmark does not exist.
    pub fn remove_bookmark(&self, bookmark_id: i64) -> anyhow::Result<()> {
        let conn = self.conn()?;
        let hash: Option<String> = conn.query_row("SELECT document_hash FROM bookmarks WHERE id = ?1", [bookmark_id], |r| r.get(0)).optional()?;
        let Some(document_hash) = hash else { return Ok(()) };
        conn.execute("DELETE FROM bookmarks WHERE id = ?1", [bookmark_id])?;
        events::emit(AppEvent::BookmarkRemoved { document_hash, bookmark_id });
        confirm("Bookmark removed");
        Ok(())
    }

    /// Ordered by page, then position.
    pub fn get_bookmarks(&self, document_hash: &str) -> anyhow::Result<Vec<Bookmark>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, document_hash, name, page_index, char_position, created_at FROM bookmarks
             WHERE document_hash = ?1 ORDER BY page_index, char_position",
        )?;
        let rows = stmt.query_map([document_hash], bookmark_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn has_bookmark(&self, document_hash: &str, page_index: usize) -> anyhow::Result<bool> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT 1 FROM bookmarks WHERE document_hash = ?1 AND page_index = ?2 LIMIT 1",
            params![document_hash, page_index as i64],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    }

    // Notes

    pub fn add_note(&self, document_hash: &str, page_index: usize, char_position: usize, text: &str) -> anyhow::Result<Note> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO notes (document_hash, page_index, char_position, text, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![document_hash, page_index as i64, char_position as i64, text, now_db()],
        )?;
        let id = conn.last_insert_rowid();
        let note = conn.query_row(
            "SELECT id, document_hash, page_index, char_position, text, created_at, updated_at FROM notes WHERE id = ?1",
            [id],
            note_from,
        )?;
        events::emit(AppEvent::NoteAdded { document_hash: document_hash.into(), page_index, text: text.into() });
        confirm(format!("Note added on page {}", page_index + 1));
        Ok(note)
    }

    /// No-op if the note does not exist.
    pub fn update_note(&self, note_id: i64, text: &str) -> anyhow::Result<()> {
        let conn = self.conn()?;
        if conn.execute("UPDATE notes SET text = ?1, updated_at = ?2 WHERE id = ?3", params![text, now_db(), note_id])? > 0 {
            confirm("Note updated");
        }
        Ok(())
    }

    /// No-op if the note does not exist.
    pub fn remove_note(&self, note_id: i64) -> anyhow::Result<()> {
        let conn = self.conn()?;
        if conn.execute("DELETE FROM notes WHERE id = ?1", [note_id])? > 0 {
            confirm("Note removed");
        }
        Ok(())
    }

    pub fn get_notes(&self, document_hash: &str) -> anyhow::Result<Vec<Note>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, document_hash, page_index, char_position, text, created_at, updated_at FROM notes
             WHERE document_hash = ?1 ORDER BY page_index, char_position",
        )?;
        let rows = stmt.query_map([document_hash], note_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // Quotes

    pub fn save_quote(&self, document_hash: &str, page_index: usize, start_pos: usize, end_pos: usize, text: &str, tags: &str) -> anyhow::Result<Quote> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO quotes (document_hash, page_index, start_pos, end_pos, text, tags, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![document_hash, page_index as i64, start_pos as i64, end_pos as i64, text, tags, now_db()],
        )?;
        let id = conn.last_insert_rowid();
        let quote = conn.query_row(
            "SELECT id, document_hash, page_index, start_pos, end_pos, text, tags, created_at FROM quotes WHERE id = ?1",
            [id],
            quote_from,
        )?;
        confirm(format!("Quote saved from page {}", page_index + 1));
        Ok(quote)
    }

    /// No-op if the quote does not exist.
    pub fn remove_quote(&self, quote_id: i64) -> anyhow::Result<()> {
        let conn = self.conn()?;
        if conn.execute("DELETE FROM quotes WHERE id = ?1", [quote_id])? > 0 {
            confirm("Quote removed");
        }
        Ok(())
    }

    pub fn get_quotes(&self, document_hash: &str) -> anyhow::Result<Vec<Quote>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, document_hash, page_index, start_pos, end_pos, text, tags, created_at FROM quotes
             WHERE document_hash = ?1 ORDER BY page_index, start_pos",
        )?;
        let rows = stmt.query_map([document_hash], quote_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // Statistics

    /// Counts for one document, or for everything when `document_hash` is `None`.
    pub fn get_counts(&self, document_hash: Option<&str>) -> anyhow::Result<AnnotationCounts> {
        let conn = self.conn()?;
        let uz0 = |r: &Row| uz(r, 0);
        let count = |table: &str| -> rusqlite::Result<usize> {
            match document_hash {
                Some(h) => conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE document_hash = ?1"), [h], uz0),
                None => conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], uz0),
            }
        };
        Ok(AnnotationCounts { bookmarks: count("bookmarks")?, notes: count("notes")?, quotes: count("quotes")? })
    }

    /// Deletes one document's annotations, or everything when `document_hash` is `None`.
    /// Returns the number of rows deleted.
    pub fn delete_all(&self, document_hash: Option<&str>) -> anyhow::Result<usize> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let mut total = 0;
        for table in ["bookmarks", "notes", "quotes"] {
            total += match document_hash {
                Some(h) => tx.execute(&format!("DELETE FROM {table} WHERE document_hash = ?1"), [h])?,
                None => tx.execute(&format!("DELETE FROM {table}"), [])?,
            };
        }
        tx.commit()?;
        Ok(total)
    }

    // Export

    pub fn export_to_text(&self, document_hash: &str, document_title: &str) -> anyhow::Result<String> {
        let out = export_annotations_text(&self.get_bookmarks(document_hash)?, &self.get_notes(document_hash)?, &self.get_quotes(document_hash)?, document_title);
        self.announce_export(document_hash, "text")?;
        Ok(out)
    }

    pub fn export_to_html(&self, document_hash: &str, document_title: &str) -> anyhow::Result<String> {
        let out = export_annotations_html(&self.get_bookmarks(document_hash)?, &self.get_notes(document_hash)?, &self.get_quotes(document_hash)?, document_title);
        self.announce_export(document_hash, "html")?;
        Ok(out)
    }

    fn announce_export(&self, document_hash: &str, format: &str) -> anyhow::Result<()> {
        let count = self.get_counts(Some(document_hash))?.total();
        events::emit(AppEvent::AnnotationExported { document_hash: document_hash.into(), format: format.into(), count });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn manager() -> (tempfile::TempDir, AnnotationManager) {
        let dir = tempfile::tempdir().unwrap();
        let m = AnnotationManager::new(dir.path().join("sub").join("annotations.db"));
        (dir, m)
    }

    #[test]
    fn bookmarks_crud_order_and_events() {
        let (_dir, m) = manager();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        let sub = events::subscribe(move |e| match e {
            AppEvent::BookmarkAdded { .. } | AppEvent::BookmarkRemoved { .. } => s.borrow_mut().push(e.clone()),
            _ => {}
        });

        let late = m.add_bookmark("abc", 5, 10, "Late").unwrap();
        let early = m.add_bookmark("abc", 2, 0, "").unwrap();
        m.add_bookmark("other", 1, 0, "Elsewhere").unwrap();
        assert_eq!(early.name, "");
        assert!(early.created_at.is_some());

        let names: Vec<_> = m.get_bookmarks("abc").unwrap().into_iter().map(|b| (b.page_index, b.char_position, b.name)).collect();
        assert_eq!(names, vec![(2, 0, "".to_string()), (5, 10, "Late".to_string())]);
        assert!(m.has_bookmark("abc", 2).unwrap());
        assert!(!m.has_bookmark("abc", 3).unwrap());

        m.remove_bookmark(late.id).unwrap();
        m.remove_bookmark(9999).unwrap(); // missing id is a no-op
        assert_eq!(m.get_bookmarks("abc").unwrap().len(), 1);
        events::unsubscribe(sub);

        let got = seen.borrow();
        assert_eq!(got.len(), 4);
        assert_eq!(got[0], AppEvent::BookmarkAdded { document_hash: "abc".into(), page_index: 5, name: "Late".into() });
        assert_eq!(got[3], AppEvent::BookmarkRemoved { document_hash: "abc".into(), bookmark_id: late.id });
    }

    #[test]
    fn notes_update_sets_updated_at() {
        let (_dir, m) = manager();
        let note = m.add_note("abc", 0, 3, "first").unwrap();
        assert!(note.updated_at.is_none());
        m.update_note(note.id, "second").unwrap();
        let notes = m.get_notes("abc").unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].text, "second");
        assert!(notes[0].updated_at.is_some());
        m.remove_note(note.id).unwrap();
        assert!(m.get_notes("abc").unwrap().is_empty());
    }

    #[test]
    fn quotes_counts_and_delete_all() {
        let (_dir, m) = manager();
        m.save_quote("abc", 1, 20, 30, "later", "tag").unwrap();
        let first = m.save_quote("abc", 1, 5, 9, "first", "").unwrap();
        m.add_bookmark("abc", 0, 0, "b").unwrap();
        m.add_note("abc", 0, 0, "n").unwrap();
        m.add_note("zzz", 0, 0, "n").unwrap();

        let starts: Vec<_> = m.get_quotes("abc").unwrap().into_iter().map(|q| q.start_pos).collect();
        assert_eq!(starts, vec![5, 20]);
        assert_eq!(m.get_counts(Some("abc")).unwrap(), AnnotationCounts { bookmarks: 1, notes: 1, quotes: 2 });
        assert_eq!(m.get_counts(None).unwrap(), AnnotationCounts { bookmarks: 1, notes: 2, quotes: 2 });

        m.remove_quote(first.id).unwrap();
        assert_eq!(m.delete_all(Some("abc")).unwrap(), 3);
        assert_eq!(m.get_counts(None).unwrap().total(), 1);
        assert_eq!(m.delete_all(None).unwrap(), 1);
    }

    #[test]
    fn export_to_text_lists_annotations_and_emits_event() {
        let (_dir, m) = manager();
        m.add_bookmark("abc", 1, 0, "Start").unwrap();
        m.save_quote("abc", 2, 4, 9, "a quote", "x, y").unwrap();
        let exported = Rc::new(RefCell::new(None));
        let e2 = exported.clone();
        let sub = events::subscribe(move |e| {
            if let AppEvent::AnnotationExported { format, count, .. } = e {
                *e2.borrow_mut() = Some((format.clone(), *count));
            }
        });
        let text = m.export_to_text("abc", "My Book").unwrap();
        events::unsubscribe(sub);
        assert!(text.starts_with("Annotations for: My Book\n========================\n"));
        assert!(text.contains("  Page 2: Start\n"));
        assert!(text.contains("    \"a quote\"\n    Tags: x, y\n"));
        assert_eq!(*exported.borrow(), Some(("text".to_string(), 2)));
    }
}
