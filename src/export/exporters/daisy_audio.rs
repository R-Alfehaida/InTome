//! DAISY 3 audio + text exporter with SMIL synchronisation.
//!
//! Writes a folder: `dtbook.xml`, `audio_NNN.wav` (one per section), `section_NNN.smil`,
//! `ncc.html` and `package.opf`.

use std::path::{Path, PathBuf};

use crate::ai::client::GeminiClient;
use crate::cancel::CancelToken;
use crate::document::Document;
use crate::export::audio::{self, AudioPipeline, SectionAudio};
use crate::export::audio_parts;
use crate::export::base::{ExportOptions, Exporter, ProgressFn, write_file};
use crate::export::exporters::daisy_text;
use crate::export::util::{escape_html as esc, flat_toc, or_default};

pub struct DaisyAudioExporter;

struct Section {
    title: String,
    text: String,
    /// 0-based; the SMIL file points at this page's `pagenum` in the DTBook.
    start_page: usize,
}

/// One section per TOC entry (its pages up to the next entry), or one per page without a TOC.
fn extract_sections(doc: &mut dyn Document, page_count: usize) -> Vec<Section> {
    let toc: Vec<(String, usize)> = flat_toc(doc.toc()).iter().map(|e| (e.title.clone(), e.page_index)).collect();
    let mut joined = |start: usize, end: usize| -> String {
        (start.min(page_count)..end.min(page_count)).map(|p| doc.page(p).text.clone()).collect::<Vec<_>>().join("\n\n")
    };
    if toc.is_empty() {
        return (0..page_count).map(|i| Section { title: format!("Page {}", i + 1), text: joined(i, i + 1), start_page: i }).collect();
    }
    (0..toc.len())
        .map(|i| {
            let end = toc.get(i + 1).map_or(page_count, |n| n.1);
            Section { title: toc[i].0.clone(), text: joined(toc[i].1, end), start_page: toc[i].1 }
        })
        .collect()
}

fn smil(title: &str, audio_file: &str, duration: f64, index: usize, start_page: usize) -> String {
    let dur = format!("{duration:.3}s");
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <smil xmlns=\"http://www.w3.org/2001/SMIL20/\">\n  <head>\n    <meta name=\"title\" content=\"{}\"/>\n  </head>\n  <body>\n\
         \x20   <seq id=\"seq_{index}\" dur=\"{dur}\">\n      <par id=\"par_{index}\">\n\
         \x20       <text src=\"dtbook.xml#page-{}\"/>\n\
         \x20       <audio src=\"{audio_file}\" clip-begin=\"0s\" clip-end=\"{dur}\"/>\n      </par>\n    </seq>\n  </body>\n</smil>",
        esc(title),
        start_page + 1
    )
}

/// `entries`: `(title, smil file, section number)` for each section that has audio.
fn ncc(title: &str, author: &str, entries: &[(String, String, usize)], total_duration: f64) -> String {
    let total = total_duration as u64;
    let time = format!("{:02}:{:02}:{:02}", total / 3600, total % 3600 / 60, total % 60);
    let (t, a) = (esc(title), esc(author));
    let mut lines = vec![
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>".to_string(),
        "<!DOCTYPE html>".into(),
        "<html xmlns=\"http://www.w3.org/1999/xhtml\">".into(),
        "<head>".into(),
        format!("<title>{t}</title>"),
        format!("<meta name=\"dc:creator\" content=\"{a}\"/>"),
        format!("<meta name=\"dc:title\" content=\"{t}\"/>"),
        format!("<meta name=\"ncc:totalTime\" content=\"{time}\"/>"),
        "<meta name=\"ncc:depth\" content=\"1\"/>".into(),
        format!("<meta name=\"ncc:files\" content=\"{}\"/>", entries.len() + 3),
        "</head>".into(),
        "<body>".into(),
    ];
    for (section_title, smil, n) in entries {
        lines.push(format!("<h1 class=\"section\"><a href=\"{smil}#par_{n}\">{}</a></h1>", esc(section_title)));
    }
    lines.push("</body>".into());
    lines.push("</html>".into());
    lines.join("
")
}

impl Exporter for DaisyAudioExporter {
    fn format_name(&self) -> &'static str {
        "DAISY (Audio)"
    }
    fn format_key(&self) -> &'static str {
        "daisy_audio"
    }
    fn file_extension(&self) -> &'static str {
        ".xml"
    }
    fn is_available(&self) -> bool {
        GeminiClient::is_available()
    }

    fn export(&self, doc: &mut dyn Document, options: &ExportOptions, cancel: &CancelToken, progress: ProgressFn) -> anyhow::Result<PathBuf> {
        let dir = daisy_text::output_dir(&options.output_path);
        daisy_text::prepare_dir(&dir)?;
        let meta = doc.metadata();
        let title = or_default(meta.title, "Untitled");
        let author = or_default(meta.author, "Unknown");
        let page_count = doc.page_count();

        let sections = extract_sections(doc, page_count);
        let pairs: Vec<(String, String)> = sections.iter().map(|s| (s.title.clone(), s.text.clone())).collect();
        let total_steps = page_count + audio::chunk_count(&pairs); // text pages, then audio chunks

        // Nothing goes into the folder until all audio is synthesised (into saved parts),
        // so a cancel or failure leaves it untouched.
        let text = daisy_text::build(doc, cancel, &mut |c, _, m| progress(c, total_steps, m))?;
        let pipeline = AudioPipeline::new(options.expressive);
        let audio = pipeline.generate_all_sections(&pairs, &options.voice, cancel, &mut |c, _, m| {
            progress(page_count + c, total_steps, m)
        })?;

        let dtbook = write_audio_package(&dir, &title, &author, &text, &sections, &audio)?;
        pipeline.discard(&audio);
        Ok(dtbook)
    }
}

/// Writes the DTBook package plus, for each section that has audio, `audio_NNN.wav` and
/// `section_NNN.smil`, then the NCC listing them. Returns the `dtbook.xml` path.
fn write_audio_package(
    dir: &Path,
    title: &str,
    author: &str,
    text: &daisy_text::DaisyText,
    sections: &[Section],
    audio: &[SectionAudio],
) -> anyhow::Result<PathBuf> {
    let dtbook = daisy_text::write_package(dir, text)?;
    let mut total_duration = 0.0;
    let mut ncc_entries = Vec::new();
    for (idx, section_audio) in audio.iter().enumerate() {
        if section_audio.is_empty() {
            continue;
        }
        let audio_name = format!("audio_{:03}.wav", idx + 1);
        let smil_name = format!("section_{:03}.smil", idx + 1);
        let duration = section_audio.duration_seconds();
        total_duration += duration;
        audio_parts::write_wav(&dir.join(&audio_name), &section_audio.parts)?;
        write_file(&dir.join(&smil_name), smil(&section_audio.title, &audio_name, duration, idx + 1, sections[idx].start_page))?;
        ncc_entries.push((section_audio.title.clone(), smil_name, idx + 1));
    }
    write_file(&dir.join("ncc.html"), ncc(title, author, &ncc_entries, total_duration))?;
    Ok(dtbook)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_lists_only_sections_with_audio() {
        use crate::cancel::CancelToken;
        let mut doc = crate::export::tests::sample_doc();
        let text = daisy_text::build(&mut doc, &CancelToken::new(), &mut |_, _, _| {}).unwrap();
        let sections = extract_sections(&mut doc, 3);
        assert_eq!(sections.iter().map(|s| s.start_page).collect::<Vec<_>>(), vec![0, 0, 1, 2]);
        let parts_dir = tempfile::tempdir().unwrap();
        let store = audio_parts::PartStore::open(parts_dir.path()).unwrap();
        // Chapter One is two half-second parts, so its WAV must join them.
        let half = vec![0u8; 24000];
        let with = |title: &str, keys: &[&str]| SectionAudio { title: title.to_string(), parts: keys.iter().map(|k| store.put(k, &half).unwrap()).collect() };
        let audio = vec![
            with("Chapter One: Beginnings", &["a", "b"]),
            with("A Section", &[]), // no text
            with("Chapter Two: Middles", &["c", "d"]),
            with("Chapter Three: Endings", &[]),
        ];
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("export-validation").join("daisy_audio");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_audio_package(&dir, "The Test Book", "A. Writer", &text, &sections, &audio).unwrap();

        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, ["audio_001.wav", "audio_003.wav", "dtbook.xml", "ncc.html", "package.opf", "section_001.smil", "section_003.smil"]);
        let ncc = std::fs::read_to_string(dir.join("ncc.html")).unwrap();
        assert!(ncc.contains("<meta name=\"ncc:totalTime\" content=\"00:00:02\"/>"));
        assert!(ncc.contains("<a href=\"section_003.smil#par_3\">Chapter Two: Middles</a>"));
        assert!(!ncc.contains("A Section"));
        let smil3 = std::fs::read_to_string(dir.join("section_003.smil")).unwrap();
        assert!(smil3.contains("<text src=\"dtbook.xml#page-2\"/>") && smil3.contains("clip-end=\"1.000s\""));
        assert_eq!(std::fs::metadata(dir.join("audio_001.wav")).unwrap().len(), 44 + 48000);
    }

    #[test]
    fn smil_points_at_the_audio_and_the_section_page() {
        let x = smil("Intro & more", "audio_001.wav", 1.5, 1, 2);
        assert!(x.contains("<meta name=\"title\" content=\"Intro &amp; more\"/>"));
        assert!(x.contains("<text src=\"dtbook.xml#page-3\"/>"));
        assert!(x.contains("<audio src=\"audio_001.wav\" clip-begin=\"0s\" clip-end=\"1.500s\"/>"));
    }
}
