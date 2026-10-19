//! Expressive narration for Gemini TTS audio export.
//!
//! Before an audio export reads a passage, a text model marks it up for the TTS
//! voice: which character says each quoted line, a short delivery style from a
//! fixed list, and inline vocal sounds. Each character gets a pitch register
//! from a casting pass over the whole book, so a listener can tell who is
//! talking without "he said". The prompts and JSON schemas are the files beside
//! this module; `docs/expressive_narration.md` explains them and the trial that
//! tuned them. Every reply is checked before any audio is made, and a passage
//! whose words came back changed is read plainly instead.

use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ai::client::{GeminiClient, text_item};
use crate::cancel::CancelToken;
use crate::config::Expressiveness;

const NARRATION_PROMPT: &str = include_str!("narration.md");
const DRAMATIC_PROMPT: &str = include_str!("dramatic.md");
const CASTING_PROMPT: &str = include_str!("casting.md");
const TURNS_SCHEMA: &str = include_str!("turns.schema.json");
const CAST_SCHEMA: &str = include_str!("cast.schema.json");

/// A book is cast in parts of at most this many characters (about 75k tokens),
/// each sent with the cast so far: a model reading a whole long book at once
/// tracks its early chapters less reliably.
pub const CAST_PART_CHARS: usize = 300_000;

/// Google's documented vocal tags, minus `<whispers>`/`<whispering>`: whispering
/// is sustained, so it is a style.
const TAGS: [&str; 38] = [
    "argh", "breath", "heavy breath", "exhales", "cackle", "cheer", "chuckle", "chuckles", "cough", "cry", "gasp", "giggle", "groan", "growl",
    "grunt", "grr", "hiss", "laugh", "laughter", "moan", "pant", "pff", "phew", "scream", "shout", "shriek", "sigh", "sighs", "sneeze",
    "snicker", "snort", "sob", "throat-clearing", "tsk", "whimper", "yawn", "short pause", "long pause",
];
const PAUSES: [&str; 2] = ["short pause", "long pause"];

/// A character who speaks in the book, from the casting pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Character {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub description: String,
    /// "low", "middle" or "high": the pitch that sets their voice apart.
    pub register: String,
}

/// A run of a passage read by one speaker with one delivery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub speaker: String,
    pub style: String,
    pub text: String,
}

fn turn(speaker: &str, style: &str, text: &str) -> Turn {
    Turn { speaker: speaker.into(), style: style.into(), text: text.into() }
}

/// The system instruction for marking up passages in `mode`.
pub fn system_prompt(mode: Expressiveness) -> String {
    match mode {
        Expressiveness::Dramatic => format!("{NARRATION_PROMPT}\n\n{DRAMATIC_PROMPT}"),
        _ => NARRATION_PROMPT.to_string(),
    }
}

pub fn casting_prompt() -> &'static str {
    CASTING_PROMPT
}

pub fn cast_schema() -> Value {
    serde_json::from_str(CAST_SCHEMA).expect("cast.schema.json is valid JSON")
}

/// The passage schema with `speaker` limited to the narrator, "unknown" and the
/// cast's ids, so the model can't invent a speaker partway through a book.
pub fn turns_schema(cast: &[Character]) -> Value {
    let mut schema: Value = serde_json::from_str(TURNS_SCHEMA).expect("turns.schema.json is valid JSON");
    let ids: Vec<&str> = ["narrator", "unknown"].into_iter().chain(cast.iter().map(|c| c.id.as_str())).collect();
    schema["properties"]["turns"]["items"]["properties"]["speaker"]["enum"] = json!(ids);
    schema
}

/// The styles the schema allows (empty excluded).
static STYLES: LazyLock<Vec<String>> = LazyLock::new(|| {
    let schema: Value = serde_json::from_str(TURNS_SCHEMA).expect("turns.schema.json is valid JSON");
    schema["properties"]["turns"]["items"]["properties"]["style"]["enum"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
});

/// Input for one casting request: the part's text, after the cast found so far.
pub fn cast_input(known: &[Character], text: &str) -> String {
    if known.is_empty() {
        return text.to_string();
    }
    format!("CAST SO FAR:\n{}\n\nTEXT:\n{text}", serde_json::to_string(known).unwrap_or_default())
}

/// The characters in a casting reply.
pub fn parse_cast(reply: &Value) -> anyhow::Result<Vec<Character>> {
    Ok(serde_json::from_value(reply["characters"].clone())?)
}

/// `known` unchanged, then each new character in `found`: a part may only add
/// characters, so ids and registers stay the same for the whole book.
pub fn merge_cast(known: &[Character], found: Vec<Character>) -> Vec<Character> {
    let mut cast = known.to_vec();
    for c in found {
        let id = c.id.trim();
        if !id.is_empty() && !matches!(id, "narrator" | "unknown") && !cast.iter().any(|k| k.id == id) {
            cast.push(Character { id: id.to_string(), ..c });
        }
    }
    cast
}

/// Input for marking up one passage.
pub fn passage_input(cast: &[Character], context: &str, passage: &str) -> String {
    let roster: Vec<String> = cast
        .iter()
        .map(|c| json!({ "id": c.id, "name": c.name, "aliases": c.aliases, "description": c.description }).to_string())
        .collect();
    format!("CAST:\n{}\n\nCONTEXT:\n{context}\n\nPASSAGE:\n{passage}", roster.join("\n"))
}

// Checks

static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^<>]{1,40})>").unwrap());
static TAG_ONLY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^<[^<>]{1,40}>$").unwrap());
static QUOTE: LazyLock<Regex> = LazyLock::new(|| Regex::new("\"[^\"]*\"|\u{201c}[^\u{201c}\u{201d}]*\u{201d}").unwrap());
static QUOTE_ONLY: LazyLock<Regex> = LazyLock::new(|| Regex::new("^(?:\"[^\"]*\"|\u{201c}[^\u{201c}\u{201d}]*\u{201d})$").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\w']+").unwrap());
static SENTENCE_END: LazyLock<Regex> = LazyLock::new(|| Regex::new("[.!?\u{201d}\"]\\s|\n").unwrap());
static FIRST_PERSON: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(I|we|me|us|my|our)\b").unwrap());

/// The words of `text` without tags, lowercased, ignoring punctuation.
fn words(text: &str) -> Vec<String> {
    let text = TAG.replace_all(text, " ").replace(['\u{2019}', '\u{2018}'], "'").to_lowercase();
    WORD.find_iter(&text).map(|m| m.as_str().to_string()).collect()
}

fn is_quote(t: &Turn) -> bool {
    QUOTE_ONLY.is_match(TAG.replace_all(&t.text, "").trim())
}

/// Checks a model's turns for `passage` and repairs what can be repaired.
/// Returns the turns to read, or why the passage must be read plainly.
pub fn check(passage: &str, turns: Vec<Turn>, cast: &[Character]) -> Result<Vec<Turn>, String> {
    // Turns become separate TTS items, so a turn boundary is a word boundary.
    let joined: Vec<&str> = turns.iter().map(|t| t.text.as_str()).collect();
    let (source, got) = (words(passage), words(&joined.join(" ")));
    if source != got {
        let at = source.iter().zip(&got).position(|(a, b)| a != b).unwrap_or(source.len().min(got.len()));
        return Err(format!(
            "words changed at word {at}: {:?} became {:?}",
            source.get(at..(at + 6).min(source.len())).unwrap_or_default().join(" "),
            got.get(at..(at + 6).min(got.len())).unwrap_or_default().join(" ")
        ));
    }
    let known_speaker = |s: &str| s == "narrator" || s == "unknown" || cast.iter().any(|c| c.id == s);
    let turns: Vec<Turn> = turns
        .into_iter()
        .filter(|t| !t.text.trim().is_empty())
        .map(|t| Turn {
            // Only tags from the list survive, spelled as listed.
            text: TAG.replace_all(&t.text, |c: &Captures| {
                let tag = c[1].trim().to_lowercase();
                if TAGS.contains(&tag.as_str()) { format!("<{tag}>") } else { String::new() }
            }).into_owned(),
            style: if STYLES.contains(&t.style) { t.style } else { String::new() },
            speaker: if known_speaker(&t.speaker) { t.speaker } else { "unknown".into() },
        })
        .collect();
    let turns = split_at_quotes(turns);
    let turns = turns.into_iter().map(|t| Turn { text: drop_misplaced_vocal_tags(&t.text), ..t }).collect();
    Ok(carry_across_speech_tags(turns))
}

/// Splits every turn at its quotation marks. Narration pieces become the plain
/// narrator; quoted pieces keep their style and speaker, and a quote given to
/// the narrator becomes "unknown". A turn with unbalanced quotes (a speech
/// running over several paragraphs) is left whole.
fn split_at_quotes(turns: Vec<Turn>) -> Vec<Turn> {
    let mut out = Vec::new();
    for t in turns {
        let text = t.text.as_str();
        if text.matches('"').count() % 2 == 1 || text.matches('\u{201c}').count() != text.matches('\u{201d}').count() {
            out.push(t);
            continue;
        }
        let speaker = if t.speaker == "narrator" { "unknown" } else { t.speaker.as_str() };
        let mut pos = 0;
        for m in QUOTE.find_iter(text) {
            if !text[pos..m.start()].trim().is_empty() {
                out.push(turn("narrator", "", &text[pos..m.start()]));
            }
            out.push(turn(speaker, &t.style, m.as_str()));
            pos = m.end();
        }
        let tail = &text[pos..];
        if !tail.trim().is_empty() {
            // A trailing tag stays with the speech it follows: "...cough." <cough>
            match out.last_mut() {
                Some(last) if pos > 0 && TAG_ONLY.is_match(tail.trim()) => last.text.push_str(tail),
                _ => out.push(turn("narrator", "", tail)),
            }
        }
    }
    out
}

/// A vocal tag outside quotes stays only right after a closing quote or in a
/// first-person sentence ("I laughed. <laugh>"); otherwise the narrator's voice
/// would make another character's sound ("He chuckled <chuckle> to himself").
/// Pauses may go anywhere.
fn drop_misplaced_vocal_tags(text: &str) -> String {
    let spans: Vec<(usize, usize)> = QUOTE.find_iter(text).map(|m| (m.start(), m.end())).collect();
    TAG.replace_all(text, |c: &Captures| {
        let (whole, start) = (c.get(0).unwrap(), c.get(0).unwrap().start());
        let keep = PAUSES.contains(&&c[1])
            || spans.iter().any(|&(s, e)| s < start && start < e)
            || spans.iter().any(|&(s, e)| text[..start].trim_end().ends_with(&text[s..e]))
            || FIRST_PERSON.is_match(SENTENCE_END.split(&text[..start]).last().unwrap_or(""));
        if keep { whole.as_str().to_string() } else { String::new() }
    })
    .into_owned()
}

/// A quote broken by a short speech tag in the same paragraph ("They want you to
/// leave," I said quietly. "The council...") is one utterance: an "unknown"
/// second half takes the first half's speaker, and an unstyled one its style.
fn carry_across_speech_tags(mut turns: Vec<Turn>) -> Vec<Turn> {
    for i in 0..turns.len().saturating_sub(2) {
        let (quote, tag, next) = (&turns[i], &turns[i + 1], &turns[i + 2]);
        if !(is_quote(quote) && is_quote(next) && !is_quote(tag) && !tag.text.contains("\n\n") && words(&tag.text).len() <= 12) {
            continue;
        }
        let (who, style) = (quote.speaker.clone(), quote.style.clone());
        let next = &mut turns[i + 2];
        if next.speaker == "unknown" && who != "unknown" {
            next.speaker = who.clone();
        }
        if !style.is_empty() && next.style.is_empty() && (next.speaker == "unknown" || next.speaker == who) {
            next.style = style;
        }
    }
    turns
}

/// The TTS text items for `turns`: each turn's style is the speaker's pitch,
/// then its delivery, such as "higher pitch, impatient". Google's guide allows
/// pitch in a style but not identity, so the cast's descriptions never reach TTS.
pub fn tts_items(turns: &[Turn], cast: &[Character]) -> Vec<Value> {
    turns
        .iter()
        .filter(|t| !t.text.trim().is_empty())
        .map(|t| {
            let pitch = match cast.iter().find(|c| c.id == t.speaker).map(|c| c.register.as_str()) {
                Some("low") => "lower pitch",
                Some("high") => "higher pitch",
                _ => "",
            };
            let style = [pitch, t.style.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
            let mut item = text_item(&t.text);
            if !style.is_empty() {
                item["annotations"] = json!([{ "type": "speech_metadata", "style": style }]);
            }
            item
        })
        .collect()
}

/// Marks up passages for one export: the model, the mode's prompt and the cast.
pub struct Narrator<'a> {
    client: &'a GeminiClient,
    model: String,
    prompt: String,
    schema: Value,
    cast: Vec<Character>,
}

impl<'a> Narrator<'a> {
    pub fn new(client: &'a GeminiClient, model: &str, mode: Expressiveness, cast: Vec<Character>) -> Self {
        Self { client, model: model.to_string(), prompt: system_prompt(mode), schema: turns_schema(&cast), cast }
    }

    /// TTS items for `passage`. A reply that fails the checks is asked for once
    /// more (a changed word is usually a one-off slip); after a second failure
    /// the passage is read plainly, never with changed words. Request errors
    /// that survive the client's retries are returned.
    pub fn items(&self, context: &str, passage: &str, cancel: Option<&CancelToken>) -> anyhow::Result<Vec<Value>> {
        let input = passage_input(&self.cast, context, passage);
        for attempt in 1..=2 {
            let reply = self.client.generate_json(&self.model, &self.prompt, &input, &self.schema, cancel)?;
            let turns = serde_json::from_value::<Vec<Turn>>(reply["turns"].clone()).map_err(|e| e.to_string());
            match turns.and_then(|t| check(passage, t, &self.cast)) {
                Ok(turns) => return Ok(tts_items(&turns, &self.cast)),
                Err(e) => log::warn!("expressive narration, attempt {attempt}: {e}"),
            }
        }
        log::warn!("reading a passage plainly: its marked-up text failed the checks twice");
        Ok(vec![text_item(passage)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn character(id: &str, register: &str) -> Character {
        Character { id: id.into(), name: id.into(), aliases: vec![], description: String::new(), register: register.into() }
    }

    fn texts(turns: &[Turn]) -> Vec<(&str, &str, &str)> {
        turns.iter().map(|t| (t.speaker.as_str(), t.style.as_str(), t.text.as_str())).collect()
    }

    #[test]
    fn prompts_and_schemas_load_and_the_speaker_list_comes_from_the_cast() {
        assert!(system_prompt(Expressiveness::Subtle).contains("### 5. Style"));
        assert!(!system_prompt(Expressiveness::Subtle).contains("Dramatic reading"));
        assert!(system_prompt(Expressiveness::Dramatic).contains("### 8. Dramatic reading"));
        let schema = turns_schema(&[character("holmes", "low")]);
        assert_eq!(schema["properties"]["turns"]["items"]["properties"]["speaker"]["enum"], json!(["narrator", "unknown", "holmes"]));
        assert_eq!(STYLES.len(), 14);
        assert_eq!(cast_schema()["properties"]["characters"]["items"]["properties"]["register"]["enum"], json!(["low", "middle", "high"]));
    }

    #[test]
    fn changed_words_fail_but_punctuation_case_and_tags_do_not() {
        let passage = "I had stopped counting and started bargaining with God.";
        let slip = vec![turn("narrator", "", "I had stopped counting and starting bargaining with God.")];
        assert!(check(passage, slip, &[]).unwrap_err().contains("\"started bargaining with god\""));
        let fine = vec![turn("narrator", "", "I had stopped counting... and STARTED <sigh> bargaining with God.")];
        assert!(check(passage, fine, &[]).is_ok());
        // Turns are separate items, so their boundary is a word boundary.
        let heading = vec![turn("narrator", "", "Chapter Three"), turn("narrator", "", "The storm.")];
        assert!(check("Chapter Three\n\nThe storm.", heading, &[]).is_ok());
    }

    #[test]
    fn unknown_tags_styles_and_speakers_are_cleaned() {
        let cast = [character("arden", "low")];
        let turns = vec![turn("ghost", "spooky", "\"Boo <applause> <Gasp>!\"")];
        let out = check("\"Boo!\"", turns, &cast).unwrap();
        assert_eq!(texts(&out), [("unknown", "", "\"Boo  <gasp>!\"")]);
    }

    #[test]
    fn narration_is_split_from_speech_and_never_styled() {
        let cast = [character("mrs_bennet", "high")];
        let turns = vec![turn("mrs_bennet", "impatient", "\u{201c}Do not you want to know?\u{201d} cried his wife, impatiently.")];
        let out = check("\u{201c}Do not you want to know?\u{201d} cried his wife, impatiently.", turns, &cast).unwrap();
        assert_eq!(texts(&out), [("mrs_bennet", "impatient", "\u{201c}Do not you want to know?\u{201d}"), ("narrator", "", " cried his wife, impatiently.")]);
    }

    #[test]
    fn a_vocal_tag_in_narration_stays_only_for_the_first_person_or_after_a_quote() {
        assert_eq!(drop_misplaced_vocal_tags("He chuckled <chuckle> to himself."), "He chuckled  to himself.");
        assert_eq!(drop_misplaced_vocal_tags("I laughed before I could stop myself, <laugh> and he grinned"), "I laughed before I could stop myself, <laugh> and he grinned");
        assert_eq!(drop_misplaced_vocal_tags("\"Ha, ha, my boy!\" <chuckle> His eyes sparkled"), "\"Ha, ha, my boy!\" <chuckle> His eyes sparkled");
        assert_eq!(drop_misplaced_vocal_tags("She paused. <long pause> Then she spoke."), "She paused. <long pause> Then she spoke.");
    }

    #[test]
    fn an_interrupted_quote_keeps_its_speaker_and_style() {
        let cast = [character("me", "middle")];
        let passage = "\"They want you to leave,\" I said quietly. \"The council.\"\n\nHe didn't move. \"Automatic.\"";
        let turns = vec![
            turn("me", "quiet and serious", "\"They want you to leave,\""),
            turn("narrator", "", " I said quietly. "),
            turn("unknown", "", "\"The council.\""),
            turn("narrator", "", "\n\nHe didn't move. "),
            turn("unknown", "", "\"Automatic.\""),
        ];
        let out = check(passage, turns, &cast).unwrap();
        assert_eq!(out[2].speaker, "me");
        assert_eq!(out[2].style, "quiet and serious");
        assert_eq!((out[4].speaker.as_str(), out[4].style.as_str()), ("unknown", ""), "a new paragraph is not carried");
    }

    #[test]
    fn tts_items_carry_pitch_then_delivery() {
        let cast = [character("mrs_bennet", "high"), character("mr_bennet", "low"), character("watson", "middle")];
        let items = tts_items(
            &[turn("mrs_bennet", "impatient", "\"Tiresome!\""), turn("mr_bennet", "", "\"Is he?\""), turn("watson", "", "\"Yes.\""), turn("narrator", "", "  "), turn("narrator", "", "He left.")],
            &cast,
        );
        assert_eq!(
            items,
            vec![
                json!({ "type": "text", "text": "\"Tiresome!\"", "annotations": [{ "type": "speech_metadata", "style": "higher pitch, impatient" }] }),
                json!({ "type": "text", "text": "\"Is he?\"", "annotations": [{ "type": "speech_metadata", "style": "lower pitch" }] }),
                json!({ "type": "text", "text": "\"Yes.\"" }),
                json!({ "type": "text", "text": "He left." }),
            ]
        );
    }

    #[test]
    fn later_parts_only_add_characters_to_the_cast() {
        let known = vec![character("holmes", "low")];
        let found = vec![character("holmes", "high"), character("irene", "high"), character("narrator", "middle"), character(" ", "low")];
        let cast = merge_cast(&known, found);
        assert_eq!(cast.iter().map(|c| (c.id.as_str(), c.register.as_str())).collect::<Vec<_>>(), [("holmes", "low"), ("irene", "high")]);
        assert!(cast_input(&known, "Text.").starts_with("CAST SO FAR:\n[{\"id\":\"holmes\""));
        assert_eq!(cast_input(&[], "Text."), "Text.");
    }
}
