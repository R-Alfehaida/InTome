//! Rich Text Format (.rtf). Port of prism/document/formats/rtf.py, which wraps
//! the `striprtf` library: text only, no heading structure.
//!
//! `rtf-parser` builds a styled document tree, which this reader does not
//! need and which misses plain-text control words real files rely on. So
//! `rtf_to_text` is a direct port of striprtf's tokenizer (same regex, same
//! destination list, same code page handling), keeping Prism's output.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use encoding_rs::Encoding;
use regex::Regex;

use crate::document::utils::{PagedDocument, normalize_newlines, page_size_of, paginate_text};
use crate::document::{Document, OpenError, OpenOptions, Page};

/// Control words that open a "destination" group whose content is not body text.
static DESTINATIONS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "aftncn aftnsep aftnsepc annotation atnauthor atndate atnicn atnid atnparent atnref atntime atrfend atrfstart author \
     background bkmkend bkmkstart blipuid buptim category colorschememapping colortbl comment company creatim datafield \
     datastore defchp defpap do doccomm docvar dptxbxtext ebcend ebcstart factoidname falt fchars ffdeftext ffentrymcr \
     ffexitmcr ffformat ffhelptext ffl ffname ffstattext file filetbl fldinst fldtype fonttbl fname fontemb fontfile footer \
     footerf footerl footerr footnote formfield ftncn ftnsep ftnsepc g generator gridtbl header headerf headerl headerr hl \
     hlfr hlinkbase hlloc hlsrc hsv htmltag info keycode keywords latentstyles lchars levelnumbers leveltext lfolevel \
     linkval list listlevel listname listoverride listoverridetable listpicture liststylename listtable lsdlockedexcept \
     macc maccPr mailmerge maln malnScr manager margPr mbar mbarPr mbaseJc mbegChr mborderBox mborderBoxPr mbox mboxPr \
     mchr mcount mctrlPr md mdeg mdegHide mden mdiff mdPr me mendChr meqArr meqArrPr mf mfName mfPr mfunc mfuncPr \
     mgroupChr mgroupChrPr mgrow mhideBot mhideLeft mhideRight mhideTop mhtmltag mlim mlimloc mlimlow mlimlowPr mlimupp \
     mlimuppPr mm mmaddfieldname mmath mmathPict mmathPr mmaxdist mmc mmcJc mmconnectstr mmconnectstrdata mmcPr mmcs \
     mmdatasource mmheadersource mmmailsubject mmodso mmodsofilter mmodsofldmpdata mmodsomappedname mmodsoname \
     mmodsorecipdata mmodsosort mmodsosrc mmodsotable mmodsoudl mmodsoudldata mmodsouniquetag mmPr mmquery mmr mnary \
     mnaryPr mnoBreak mnum mobjDist moMath moMathPara moMathParaPr mopEmu mphant mphantPr mplcHide mpos mr mrad mradPr \
     mrPr msepChr mshow mshp msPre msPrePr msSub msSubPr msSubSup msSubSupPr msSup msSupPr mstrikeBLTR mstrikeH \
     mstrikeTLBR mstrikeV msub msubHide msup msupHide mtransp mtype mvertJc mvfmf mvfml mvtof mvtol mzeroAsc mzeroDesc \
     mzeroWid nesttableprops nextfile nonesttables objalias objclass objdata object objname objsect objtime oldcprops \
     oldpprops oldsprops oldtprops oleclsid operator panose password passwordhash pgp pgptbl picprop pict pn pnseclvl \
     pntext pntxta pntxtb printim private propname protend protstart protusertbl pxe result revtbl revtim rsidtbl rxe shp \
     shpgrp shpinst shppict shprslt shptxt sn sp staticval stylesheet subject sv svb tc template themedata title txe ud \
     upr userprops wgrffmtfilter windowcaption writereservation writereservhash xe xform xmlattrname xmlattrvalue \
     xmlclose xmlname xmlnstbl xmlopen"
        .split_whitespace()
        .collect()
});

static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\\([a-z]{1,32})(-?[0-9]{1,10})?[ ]?|\\'([0-9a-f]{2})|\\([^a-z])|([{}])|[\r\n]+|(.)").unwrap()
});
static HYPERLINKS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(\{\\field\{\s*\\\*\\fldinst\{.*HYPERLINK\s(".*")\}{2}\s*\{.*?\s+(.*?)\}{2,3})"#).unwrap()
});
static FONTTABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\\f([0-9]+).*?\\fcharset([0-9]+).*?([^;]+);").unwrap());
static FONTTABLE_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^{}]*\\fonttbl").unwrap());

/// Output for a control word, or for a control symbol when the key is one char.
fn special(word: &str) -> Option<&'static str> {
    Some(match word {
        "par" => "\n",
        "sect" | "page" => "\n\n",
        "line" | "row" | "\n" => "\n",
        "tab" => "\t",
        "emdash" => "\u{2014}",
        "endash" => "\u{2013}",
        "emspace" => "\u{2003}",
        "enspace" => "\u{2002}",
        "qmspace" => "\u{2005}",
        "bullet" => "\u{2022}",
        "lquote" => "\u{2018}",
        "rquote" => "\u{2019}",
        "ldblquote" => "\u{201C}",
        "rdblquote" => "\u{201D}",
        "cell" | "nestcell" => "|",
        "~" => "\u{a0}",
        "\r" => "\r",
        "{" => "{",
        "}" => "}",
        "\\" => "\\",
        "-" => "\u{ad}",
        "_" => "\u{2011}",
        _ => return None,
    })
}

/// Code page number (`\ansicpgN`) to an encoding. Unknown OEM pages fall back to
/// Windows-1252; anything else unknown to UTF-8, as striprtf does.
fn codepage(n: u32) -> &'static Encoding {
    match n {
        932 => encoding_rs::SHIFT_JIS,
        936 => encoding_rs::GBK,
        949 => encoding_rs::EUC_KR,
        950 => encoding_rs::BIG5,
        10000 => encoding_rs::MACINTOSH,
        10007 => encoding_rs::X_MAC_CYRILLIC,
        65001 => encoding_rs::UTF_8,
        866 => encoding_rs::IBM866,
        _ => Encoding::for_label(format!("windows-{n}").as_bytes())
            .or_else(|| Encoding::for_label(format!("cp{n}").as_bytes()))
            .unwrap_or(if n < 1000 { encoding_rs::WINDOWS_1252 } else { encoding_rs::UTF_8 }),
    }
}

/// `\fcharsetN` to an encoding, or `default` for charsets without one here.
fn charset(n: u32, default: &'static Encoding) -> &'static Encoding {
    match n {
        0 | 42 => encoding_rs::WINDOWS_1252,
        77 => encoding_rs::MACINTOSH,
        84 => encoding_rs::X_MAC_CYRILLIC,
        128 => encoding_rs::SHIFT_JIS,
        129 => encoding_rs::EUC_KR,
        134 => encoding_rs::GBK,
        136 => encoding_rs::BIG5,
        161 => encoding_rs::WINDOWS_1253,
        162 => encoding_rs::WINDOWS_1254,
        163 => encoding_rs::WINDOWS_1258,
        177 => encoding_rs::WINDOWS_1255,
        178 => encoding_rs::WINDOWS_1256,
        186 => encoding_rs::WINDOWS_1257,
        204 => encoding_rs::WINDOWS_1251,
        222 => encoding_rs::WINDOWS_874,
        238 => encoding_rs::WINDOWS_1250,
        _ => default,
    }
}

/// Drops `\pict` groups that carry binary data (`\binN`), which would otherwise
/// swallow the tokenizer.
fn remove_pict_groups(rtf: &str) -> String {
    if !rtf.contains("\\pict") || !rtf.contains("\\bin") {
        return rtf.to_string();
    }
    let chars: Vec<char> = rtf.chars().collect();
    let starts = |i: usize, pat: &str| chars[i..].iter().zip(pat.chars()).filter(|(a, b)| *a == b).count() == pat.chars().count();
    let mut out = String::new();
    let (mut i, mut in_pict) = (0, false);
    while i < chars.len() {
        if !in_pict && starts(i, "\\pict") {
            in_pict = true;
            i += 5;
            continue;
        }
        if in_pict {
            if starts(i, "\\bin") {
                i += 4;
                let mut digits = String::new();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    digits.push(chars[i]);
                    i += 1;
                }
                i += digits.parse::<usize>().unwrap_or(0);
                continue;
            } else if chars[i] == '}' {
                in_pict = false;
                i += 1;
                continue;
            }
        }
        if !in_pict {
            out.push(chars[i]);
        }
        i += 1;
    }
    out
}

/// The `{\fonttbl ...}` group, or "" when absent.
fn font_table_group(text: &str) -> &str {
    let Some(start) = FONTTABLE_START.find(text) else { return "" };
    let mut depth = 1;
    for (i, c) in text[start.end()..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => continue,
        }
        if depth == 0 {
            return &text[start.start()..start.end() + i + 1];
        }
    }
    &text[start.start()..]
}

/// Converts RTF source (one char per byte) to plain text.
fn rtf_to_text(input: &str) -> String {
    let text = remove_pict_groups(input);
    let text = HYPERLINKS.replace_all(&text, "${1}(${2})");
    let mut encoding = encoding_rs::WINDOWS_1252;
    let fonts: HashMap<String, &'static Encoding> = FONTTABLE
        .captures_iter(font_table_group(&text))
        .map(|c| (c[1].to_string(), charset(c[2].parse().unwrap_or(u32::MAX), encoding)))
        .collect();

    let mut stack: Vec<(usize, bool, bool)> = Vec::new();
    let (mut ignorable, mut suppress) = (false, false);
    let (mut ucskip, mut curskip) = (1usize, 0usize);
    let mut hexes: Vec<u8> = Vec::new();
    // UTF-16 so `\u` surrogate pairs recombine into one character.
    let mut out: Vec<u16> = Vec::new();
    let (mut depth, mut in_document) = (0i32, false);
    let mut current_font: Option<String> = None;

    for caps in PATTERN.captures_iter(&text) {
        let (word, arg) = (caps.get(1).map(|m| m.as_str()), caps.get(2).map(|m| m.as_str()));
        let (hex, sym) = (caps.get(3).map(|m| m.as_str()), caps.get(4).map(|m| m.as_str()));
        let (brace, tchar) = (caps.get(5).map(|m| m.as_str()), caps.get(6).map(|m| m.as_str()));
        if hex.is_none() && !hexes.is_empty() {
            let enc = current_font.as_ref().and_then(|f| fonts.get(f)).copied().unwrap_or(encoding);
            out.extend(enc.decode_without_bom_handling(&hexes).0.encode_utf16());
            hexes.clear();
        }
        if let Some(b) = brace {
            curskip = 0;
            if b == "{" {
                depth += 1;
                in_document = true;
                stack.push((ucskip, ignorable, suppress));
            } else {
                depth -= 1;
                match stack.pop() {
                    Some((u, i, s)) => (ucskip, ignorable, suppress) = (u, i, s),
                    None => (ucskip, ignorable) = (0, true),
                }
                if in_document && depth <= 0 {
                    break;
                }
            }
        } else if let Some(c) = sym {
            curskip = 0;
            if let Some(s) = special(c) {
                if !ignorable {
                    out.extend(s.encode_utf16());
                }
            } else if c == "*" {
                ignorable = true;
            }
        } else if let Some(w) = word {
            curskip = 0;
            if DESTINATIONS.contains(w) {
                ignorable = true;
            } else if w == "ansicpg" {
                encoding = codepage(arg.and_then(|a| a.parse().ok()).unwrap_or(0));
            }
            if ignorable || suppress {
                continue;
            }
            if let Some(s) = special(w) {
                out.extend(s.encode_utf16());
            } else {
                match w {
                    "uc" => ucskip = arg.and_then(|a| a.parse().ok()).unwrap_or(ucskip),
                    "u" => {
                        if let Some(n) = arg.and_then(|a| a.parse::<i32>().ok()) {
                            out.push(if n < 0 { n + 0x10000 } else { n } as u16);
                        }
                        curskip = ucskip;
                    }
                    "f" => current_font = arg.map(str::to_string),
                    "fonttbl" | "colortbl" => suppress = true,
                    _ => {}
                }
            }
        } else if let Some(h) = hex {
            if curskip > 0 {
                curskip -= 1;
            } else if !ignorable {
                hexes.push(u8::from_str_radix(h, 16).unwrap_or(b'?'));
            }
        } else if let Some(t) = tchar {
            if curskip > 0 {
                curskip -= 1;
            } else if !ignorable && !suppress {
                out.extend(t.encode_utf16());
            }
        }
    }
    String::from_utf16_lossy(&out)
}

pub fn open(path: &Path, opts: &OpenOptions) -> Result<Box<dyn Document>, OpenError> {
    // RTF control words are 7-bit ASCII; reading each byte as one char (latin-1)
    // never fails, and the `\'xx` escapes are decoded with the file's own code page.
    let raw: String = std::fs::read(path)?.iter().map(|&b| char::from(b)).collect();
    let text = rtf_to_text(&normalize_newlines(&raw)).trim().to_string();
    let text = if text.is_empty() { "(Empty document)".to_string() } else { text };
    let pages = paginate_text(&text, page_size_of(opts)).into_iter().map(Page::plain).collect();
    Ok(Box::new(PagedDocument::new(path, "RTF", PagedDocument::file_name(path), pages, Vec::new())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_paragraphs_unicode_and_code_page_escapes() {
        let rtf = r"{\rtf1\ansi\ansicpg1252\deff0{\fonttbl{\f0\fcharset0 Arial;}}{\colortbl;\red255\green0\blue0;}\f0 Hello \b bold\b0 \u960? caf\'e9\par Second \u-10179?\u-8704? end}";
        assert_eq!(rtf_to_text(rtf), "Hello bold\u{3c0} caf\u{e9}\nSecond \u{1F600} end");
    }

    #[test]
    fn skips_destinations_and_stops_at_the_closing_brace() {
        let rtf = r"{\rtf1{\*\generator Word;}{\info{\title Secret}}Visible\tab text\~here\line next}trailing junk";
        assert_eq!(rtf_to_text(rtf), "Visible\ttext\u{a0}here\nnext");
    }

    #[test]
    fn hex_escapes_use_the_font_charset_and_hyperlinks_keep_their_target() {
        let rtf = r"{\rtf1\ansi{\fonttbl{\f1\fcharset204 Arial;}}\f1 \'cf\'f0\'e8\'e2\'e5\'f2}";
        assert_eq!(rtf_to_text(rtf), "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}");
        let link = r#"{\rtf1 {\field{\*\fldinst{HYPERLINK "http://x.org"}}{\fldrslt site}} after}"#;
        assert_eq!(rtf_to_text(link), "site(\"http://x.org\") after");
    }
}
