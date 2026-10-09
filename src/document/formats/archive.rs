//! ZIP and RAR archives of documents. Port of prism/document/formats/archive.py.
//!
//! Prism lists the members whose extension is a supported format, offers a
//! "Select Document" dialog when there are several (first one when it cannot
//! show one), extracts that one member to a temp folder and shows it through the
//! normal format handlers. Here [`list_documents`] and [`open_member`] let the GUI
//! run that dialog; `open_zip`/`open_rar` take the first member like Prism's
//! headless fallback.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::document::registry;
use crate::document::{
    Capabilities, DocMetadata, Document, FormField, OpenError, OpenOptions, Page, SearchResult, TocEntry,
};
use crate::structured_text::elements::SemanticElement;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Zip,
    Rar,
}

fn kind_of(path: &Path) -> Result<Kind, OpenError> {
    match path.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref() {
        Some("zip") => Ok(Kind::Zip),
        Some("rar") => Ok(Kind::Rar),
        other => Err(OpenError::Other(format!("Unsupported archive format: {}", other.map(|e| format!(".{e}")).unwrap_or_default()))),
    }
}

fn other(path: &Path, e: impl std::fmt::Display) -> OpenError {
    OpenError::Other(format!("Could not read the archive {}: {e}", path.file_name().unwrap_or_default().to_string_lossy()))
}

/// Members with a supported extension, skipping dot-files, in archive order.
fn select_documents(names: &[String], supported: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|n| {
            let base = n.rsplit(['/', '\\']).next().unwrap_or("");
            let ext = base.rfind('.').filter(|&i| i > 0).map(|i| base[i..].to_lowercase()).unwrap_or_default();
            supported.contains(&ext.as_str()) && !base.starts_with('.')
        })
        .cloned()
        .collect()
}

fn member_names(path: &Path, kind: Kind) -> Result<Vec<String>, OpenError> {
    match kind {
        Kind::Zip => {
            let mut zip = zip::ZipArchive::new(fs::File::open(path)?).map_err(|e| other(path, e))?;
            (0..zip.len())
                .map(|i| zip.by_index(i).map(|f| f.name().to_string()).map_err(|e| other(path, e)))
                .collect()
        }
        Kind::Rar => {
            let mut names = Vec::new();
            for entry in unrar::Archive::new(path).open_for_listing().map_err(|e| other(path, e))? {
                let entry = entry.map_err(|e| other(path, e))?;
                if !entry.is_directory() {
                    names.push(entry.filename.to_string_lossy().replace('\\', "/"));
                }
            }
            Ok(names)
        }
    }
}

/// The documents inside the archive that InTome can open, in archive order.
pub fn list_documents(path: &Path) -> Result<Vec<String>, OpenError> {
    let names = member_names(path, kind_of(path)?)?;
    Ok(select_documents(&names, &registry::available_extensions()))
}

fn read_member(path: &Path, kind: Kind, member: &str) -> Result<Vec<u8>, OpenError> {
    match kind {
        Kind::Zip => {
            let mut zip = zip::ZipArchive::new(fs::File::open(path)?).map_err(|e| other(path, e))?;
            let mut file = zip.by_name(member).map_err(|e| other(path, e))?;
            let mut data = Vec::new();
            file.read_to_end(&mut data)?;
            Ok(data)
        }
        Kind::Rar => {
            let mut archive = unrar::Archive::new(path).open_for_processing().map_err(|e| other(path, e))?;
            while let Some(header) = archive.read_header().map_err(|e| other(path, e))? {
                if header.entry().filename.to_string_lossy().replace('\\', "/") == member {
                    return header.read().map(|(data, _)| data).map_err(|e| other(path, e));
                }
                archive = header.skip().map_err(|e| other(path, e))?;
            }
            Err(other(path, format!("{member} was not found")))
        }
    }
}

/// A path under the extraction folder that cannot escape it.
fn safe_relative(member: &str) -> PathBuf {
    member
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != ".." && !s.contains(':'))
        .collect()
}

pub struct ArchiveDocument {
    // Dropped before `_tmp`: the inner document may hold the extracted file open.
    inner: Option<Box<dyn Document>>,
    _tmp: tempfile::TempDir,
    path: PathBuf,
    empty: Page,
}

fn open_kind(path: &Path, kind: Kind, member: Option<&str>, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let docs = list_documents(path)?;
    let selected = match member {
        Some(m) => Some(m.to_string()),
        None => docs.first().cloned(),
    };
    let tmp = tempfile::Builder::new().prefix("intome_archive_").tempdir()?;
    let inner = match selected {
        None => None,
        Some(name) => {
            let relative = safe_relative(&name);
            if relative.as_os_str().is_empty() {
                return Err(other(path, format!("{name} is not a valid member name")));
            }
            let target = tmp.path().join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, read_member(path, kind, &name)?)?;
            Some(registry::open_document(&target, opts)?)
        }
    };
    Ok(Box::new(ArchiveDocument {
        inner,
        _tmp: tmp,
        path: path.to_path_buf(),
        empty: Page::plain("(No supported documents found in archive)"),
    }))
}

pub fn open_zip(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    open_kind(path, Kind::Zip, None, opts)
}

pub fn open_rar(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    open_kind(path, Kind::Rar, None, opts)
}

/// Opens one chosen member (one of [`list_documents`]) of a `.zip` or `.rar`.
pub fn open_member(path: &Path, member: &str, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    open_kind(path, kind_of(path)?, Some(member), opts)
}

impl Document for ArchiveDocument {
    fn path(&self) -> &Path {
        &self.path
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.as_ref().map_or(Capabilities::empty(), |d| d.capabilities())
    }
    fn toc(&self) -> &[TocEntry] {
        self.inner.as_ref().map_or(&[], |d| d.toc())
    }
    fn metadata(&self) -> DocMetadata {
        let archive = self.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        match &self.inner {
            Some(d) => {
                let mut meta = d.metadata();
                meta.extra.push(("archive".into(), archive));
                meta
            }
            None => DocMetadata { title: archive, format: "Archive".into(), ..Default::default() },
        }
    }
    fn page_count(&self) -> usize {
        self.inner.as_ref().map_or(1, |d| d.page_count())
    }
    fn page(&mut self, index: usize) -> &Page {
        match &mut self.inner {
            Some(d) => d.page(index),
            None => &self.empty,
        }
    }
    fn set_ocr_result(&mut self, index: usize, text: String, elements: Vec<SemanticElement>) {
        if let Some(d) = &mut self.inner {
            d.set_ocr_result(index, text, elements);
        }
    }
    fn ocr_page_indices(&mut self) -> Vec<usize> {
        self.inner.as_mut().map(|d| d.ocr_page_indices()).unwrap_or_default()
    }
    fn form_fields(&mut self) -> Vec<FormField> {
        self.inner.as_mut().map(|d| d.form_fields()).unwrap_or_default()
    }
    fn as_pdf(&mut self) -> Option<&mut super::pdf::PdfDocument> {
        self.inner.as_mut().and_then(|d| d.as_pdf())
    }
    fn search(&mut self, query: &str) -> Vec<SearchResult> {
        self.inner.as_mut().map(|d| d.search(query)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_supported_members_skipping_dotfiles_in_archive_order() {
        let names: Vec<String> = ["docs/", "b.TXT", ".hidden.txt", "sub/.DS_Store.md", "pic.xyz", "sub/report.docx", "README"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(select_documents(&names, &[".txt", ".docx", ".md"]), vec!["b.TXT", "sub/report.docx"]);
    }

    #[test]
    fn extraction_path_cannot_escape_the_temp_folder() {
        assert_eq!(safe_relative("../../evil/C:/x.txt"), PathBuf::from("evil").join("x.txt"));
        assert_eq!(safe_relative("a\\b/c.md"), PathBuf::from("a").join("b").join("c.md"));
    }

    fn zip_of(dir: &Path, entries: &[&str]) -> PathBuf {
        use std::io::Write;
        let path = dir.join("t.zip");
        let mut w = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        for name in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"x").unwrap();
        }
        w.finish().unwrap();
        path
    }

    #[test]
    fn opening_a_zip_shows_the_first_supported_member_with_archive_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = zip_of(dir.path(), &["notes.bin", ".hidden.png", "pics/scan.png"]);
        let mut doc = open_zip(&path, &OpenOptions::default()).unwrap();
        assert_eq!(doc.capabilities(), Capabilities::NEEDS_OCR | Capabilities::IMAGES);
        assert_eq!(doc.page_count(), 1);
        assert!(doc.page(0).text.starts_with("Image file: scan.png\n"));
        let meta = doc.metadata();
        assert_eq!((meta.title.as_str(), meta.format.as_str()), ("scan.png", "Image"));
        assert_eq!(meta.extra, vec![("archive".to_string(), "t.zip".to_string())]);
    }

    #[test]
    fn a_zip_without_supported_members_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = zip_of(dir.path(), &["data.bin", "readme"]);
        let mut doc = open_zip(&path, &OpenOptions::default()).unwrap();
        assert_eq!((doc.page_count(), doc.page(0).text.as_str()), (1, "(No supported documents found in archive)"));
        assert_eq!(doc.capabilities(), Capabilities::empty());
        let meta = doc.metadata();
        assert_eq!((meta.title.as_str(), meta.format.as_str()), ("t.zip", "Archive"));
    }

    #[test]
    fn zip_listing_reads_member_names_in_order() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.zip");
        let mut w = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        for name in ["z.txt", "a.txt"] {
            w.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b"x").unwrap();
        }
        w.finish().unwrap();
        assert_eq!(member_names(&path, Kind::Zip).unwrap(), vec!["z.txt", "a.txt"]);
        assert_eq!(read_member(&path, Kind::Zip, "a.txt").unwrap(), b"x");
    }
}
