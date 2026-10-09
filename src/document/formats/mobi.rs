//! MOBI / AZW / AZW3 ebooks. Port of prism/document/formats/mobi.py.
//!
//! Prism unpacks with the Python `mobi` package (KindleUnpack) and parses the
//! HTML it writes. Here the `mobi` crate supplies the PalmDB records and headers,
//! and the text is decoded in this file: the crate's text decoder keeps each
//! record's trailing data and truncates overlapping PalmDOC back-references,
//! both of which corrupt real Kindle files.
//!
//! The text is the book's raw markup (KindleUnpack additionally rewrites image
//! and link targets, which do not affect the text).

use std::path::Path;

use mobi::Mobi;
use mobi::headers::{Compression, Encryption, TextEncoding};

use super::word::{PagedDoc, effective_page_size, file_name};
use crate::document::utils::paginate_text;
use crate::document::{DocMetadata, Document, OpenError, OpenOptions, Page};
use crate::structured_text::html_parser::parse_html;

/// PalmDOC LZ77. A back-reference may overlap the bytes it is producing, so
/// it has to be copied one byte at a time.
fn palmdoc_decompress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        i += 1;
        match c {
            0x00 | 0x09..=0x7f => out.push(c),
            0x01..=0x08 => {
                let end = (i + c as usize).min(data.len());
                out.extend_from_slice(&data[i..end]);
                i = end;
            }
            0x80..=0xbf => {
                let Some(&next) = data.get(i) else { break };
                i += 1;
                let pair = (((c as usize) << 8) | next as usize) & 0x3fff;
                let (distance, length) = (pair >> 3, (pair & 7) + 3);
                if distance == 0 || distance > out.len() {
                    continue;
                }
                for _ in 0..length {
                    out.push(out[out.len() - distance]);
                }
            }
            0xc0..=0xff => out.extend_from_slice(&[b' ', c ^ 0x80]),
        }
    }
    out
}

/// Bytes of one backward-encoded varint at the end of `data[..size]`.
fn trailing_entry_size(data: &[u8], mut size: usize) -> usize {
    let (mut bitpos, mut result) = (0, 0usize);
    while size > 0 {
        let v = data[size - 1];
        result |= ((v & 0x7f) as usize) << bitpos;
        bitpos += 7;
        size -= 1;
        if v & 0x80 != 0 || bitpos >= 28 {
            break;
        }
    }
    result
}

/// Length of the trailing data a text record carries after its compressed text,
/// as described by the header's extra-data flags.
fn trailing_data_size(record: &[u8], flags: u16) -> usize {
    let mut total = 0;
    let mut rest = flags >> 1;
    while rest != 0 {
        if rest & 1 != 0 {
            total += trailing_entry_size(record, record.len().saturating_sub(total));
        }
        rest >>= 1;
    }
    if flags & 1 != 0 && record.len() > total {
        total += ((record[record.len() - total - 1] & 3) + 1) as usize;
    }
    total.min(record.len())
}

fn decode(bytes: &[u8], encoding: TextEncoding) -> String {
    match encoding {
        TextEncoding::CP1252 => encoding_rs::WINDOWS_1252.decode(bytes).0.into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The header's extra-data flags (which trailing data each text record carries).
/// The crate zero-fills record 0 once the headers are parsed, so read the field from the file.
fn extra_data_flags(path: &Path, book: &Mobi) -> u16 {
    use std::io::{Read, Seek, SeekFrom};
    let (mobi, first) = (&book.metadata.mobi, book.metadata.records.records.first());
    let Some(first) = first.filter(|_| mobi.header_length >= 0xE4 && mobi.gen_version >= 5) else { return 0 };
    let mut bytes = [0u8; 2];
    let read = std::fs::File::open(path).and_then(|mut f| {
        f.seek(SeekFrom::Start(u64::from(first.offset) + 0xF2))?;
        f.read_exact(&mut bytes)
    });
    if read.is_ok() { u16::from_be_bytes(bytes) } else { 0 }
}

fn book_markup(path: &Path, book: &Mobi) -> Result<String, OpenError> {
    let meta = &book.metadata;
    if meta.palmdoc.encryption != Encryption::No {
        return Err(OpenError::Other("This MOBI file is encrypted (DRM) and cannot be opened.".into()));
    }
    let records = book.raw_records();
    let records = records.records();
    let extra_flags = extra_data_flags(path, book);
    let text_records = records.iter().skip(1).take(meta.palmdoc.record_count as usize);

    let mut raw = Vec::new();
    for r in text_records {
        let data = &r.content[..r.content.len() - trailing_data_size(r.content, extra_flags)];
        match meta.palmdoc.compression {
            Compression::No => raw.extend_from_slice(data),
            Compression::PalmDoc => raw.extend(palmdoc_decompress(data)),
            // shortcut: Huffman/CDIC books use the crate's decoder, which keeps trailing data; rare, upgrade if one shows up.
            Compression::Huff => return Ok(book.content_as_string_lossy()),
        }
    }
    Ok(decode(&raw, meta.mobi.text_encoding))
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    let book = Mobi::from_path(path)
        .map_err(|e| OpenError::Other(format!("Could not read {} as a MOBI ebook: {e}", file_name(path))))?;
    let html = book_markup(path, &book)?;
    let text = if html.is_empty() { "(Could not extract text from MOBI file)".to_string() } else { parse_html(&html).text };
    let pages = paginate_text(&text, effective_page_size(opts)).into_iter().map(Page::plain).collect();
    let meta = DocMetadata { title: file_name(path), format: "MOBI".into(), ..Default::default() };
    Ok(Box::new(PagedDoc::new(path, pages, Vec::new(), meta)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palmdoc_expands_literals_spaces_and_overlapping_references() {
        // "ab" as a counted literal, then distance 2 length 6 (overlaps its own output), then 0xC1 = " A".
        let data = [0x02, b'a', b'b', 0x80, 0x13, 0xC1];
        assert_eq!(palmdoc_decompress(&data), b"abababab A");
    }

    #[test]
    fn trailing_data_covers_multibyte_overlap_and_indexed_entries() {
        // Text, the multibyte overlap (continuation byte 0x82 + count byte 0x01), then a 1-byte index entry 0x81.
        let record = [b'h', b'i', 0xE2, 0x82, 0x01, 0x81];
        // The 0x81 entry is 1 byte long; the count byte (0x01 & 3) + 1 then removes 2 more.
        assert_eq!(trailing_data_size(&record, 0b11), 3);
        assert_eq!(trailing_data_size(&record, 0), 0);
    }
}
