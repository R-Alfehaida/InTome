//! System prompts for AI OCR operations, copied verbatim from Prism.
//!
//! The OCR tag vocabulary in [`DEFAULT_OCR_PROMPT`] is what
//! [`crate::ai::structure::parse_ocr_response`] parses; change both together.

pub const DEFAULT_OCR_PROMPT: &str = r#"You are an expert document OCR system. Extract ALL text from this page image with perfect accuracy.

Rules:
- Transcribe every word, number, and symbol exactly as it appears
- Preserve document structure: mark headings with [H1]...[/H1] through [H6]...[/H6]
- Mark tables with [TABLE]...[/TABLE], rows with [ROW]...[/ROW], cells with [CELL]...[/CELL]
- Mark lists with [LIST]...[/LIST] and items with [ITEM]...[/ITEM]
- Mark links with [LINK url="..."]...[/LINK]
- Mark figures/images with [FIGURE alt="description"]
- For multi-column layouts, read in logical reading order (not left-to-right across columns)
- For RTL text (Arabic, Hebrew), preserve the correct reading direction
- If text is unclear, provide your best transcription with [UNCLEAR]...[/UNCLEAR] marker
- Do NOT summarize, paraphrase, or skip any content
"#;

pub const MANGA_PROMPT: &str = r#"You are an expert manga/comic transcription system.

Rules:
- Describe each panel in reading order (right-to-left for manga, left-to-right for Western comics)
- Transcribe ALL dialogue bubbles, marking speakers when identifiable: [SPEAKER: name] "dialogue"
- Describe visual action and scene setting between dialogue: [SCENE: description]
- Note sound effects: [SFX: text]
- Mark page transitions: [PAGE n]
"#;

pub const FORM_FIELD_LABELING_PROMPT: &str = r#"You are analyzing a fillable PDF form. I will provide:
1. An image of the form page
2. A list of form fields with their positions (x0, y0, x1, y1) and internal names

For each field, determine what it represents based on its visual context -- the nearby text labels, column headers, row labels, or surrounding content.

Return a JSON array where each entry has:
- "field_name": the internal field name I provided
- "label": a clear, descriptive, screen-reader-friendly label
- "group": optional grouping (e.g., "Personal Information", "Employment History")
- "context": brief description of what value is expected

Distinguishing fields that share a row or criterion:
- Many forms lay out two or more choices side by side under column headers
  (for example a "Yes" column and a "No" column, each with its own checkbox
  on the same row). Two checkboxes in the same row are NOT the same field --
  an unchecked box means "not answered", not "No", so each box needs a label
  that names BOTH the column it belongs to AND the row's criterion.
- When a field sits under a column header, prefix its label with that header
  text followed by a colon, then the row's criterion text.
  Example for a Yes/No checklist row:
    "Yes: All ideas are relevant to the essay topic"
    "No: All ideas are relevant to the essay topic"
- Use the column header text visible in the image (and each field's x/y
  position relative to the headers) to decide which column a field is in, so
  that every field gets a unique, unambiguous label.

Return ONLY the JSON array, no other text.
"#;

pub const FLAT_FORM_DISCOVERY_PROMPT: &str = r#"You are analyzing an image of a printed document page that is NOT an interactive
form -- it has no fillable widgets. Find every place a person is expected to fill
in, and classify what KIND of input each one is. Use visual context the same way
a sighted reader would.

Be conservative -- report ONLY genuine answer spaces. Do NOT report:
- underlined or bold HEADINGS and titles
- underlines used purely for emphasis on words that already have text
- table borders, rules, or decorative lines that are not answer spaces
- spaces that already contain an answer

Return a JSON array. Each entry has:
- "label": a clear, descriptive, screen-reader-friendly label inferred from the
  nearby printed text (e.g. "Full Name", "Marital Status", "Date (DD/MM/YYYY)")
- "group": optional grouping (e.g. "Personal Details", "Emergency Contact")
- "context": brief description of the value expected
- "field_type": one of:
    "text"      -- a blank line, underscores, or an empty box/cell to write in
    "date"      -- a blank for a date
    "signature" -- a blank for a signature
    "checkbox"  -- a SINGLE tick box that is checked on or off
    "choice"    -- pick ONE of several tick boxes (e.g. YES / NO boxes, or a row
                   of boxes each labelled with an option)
    "select"    -- pick ONE of several printed words shown as alternatives,
                   typically separated by slashes (e.g. "Single / Married /
                   Divorced", or "Male / Female")
- "rect": [x0, y0, x1, y1] as integers 0-1000 scaled to the image (x from the
  LEFT edge, y from the TOP edge, order horizontal-first). For "text"/"date"/
  "signature" this is the writable space; for "checkbox" this is the single box.
  Omit for "choice"/"select".
- "options": REQUIRED for "choice" and "select" only -- a list of
  {"label": <option text>, "rect": [x0,y0,x1,y1]} where rect (0-1000) is the
  tick box to mark (for "choice") or the printed word to mark (for "select").

Example checkbox row "wife working: [ ] YES  [ ] NO" ->
  {"label":"Is wife working","field_type":"choice",
   "options":[{"label":"YES","rect":[..]},{"label":"NO","rect":[..]}]}
Example "Gender: Male / Female" ->
  {"label":"Gender","field_type":"select",
   "options":[{"label":"Male","rect":[..]},{"label":"Female","rect":[..]}]}

If the page has no answer spaces, return an empty array.

Return ONLY the JSON array, no other text.
"#;

/// The final OCR prompt: the override if set, else the manga or default
/// prompt, with any user instructions appended.
pub fn get_ocr_prompt(manga_mode: bool, system_prompt_override: &str, user_instructions: &str) -> String {
    let base = if !system_prompt_override.is_empty() {
        system_prompt_override
    } else if manga_mode {
        MANGA_PROMPT
    } else {
        DEFAULT_OCR_PROMPT
    };
    if user_instructions.is_empty() {
        base.to_string()
    } else {
        format!("{}\n\nAdditional instructions:\n{user_instructions}", base.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_assembly_picks_base_and_appends_instructions() {
        assert_eq!(
            get_ocr_prompt(false, "Read it.\n\n", "Keep page numbers."),
            "Read it.\n\nAdditional instructions:\nKeep page numbers."
        );
        assert_eq!(get_ocr_prompt(true, "Read it.", ""), "Read it.");
        assert!(get_ocr_prompt(true, "", "").starts_with("You are an expert manga/comic transcription system.\n\nRules:\n"));
        let default = get_ocr_prompt(false, "", "Be brief.");
        assert!(default.starts_with("You are an expert document OCR system. Extract ALL text from this page image with perfect accuracy.\n"));
        assert!(default.ends_with("- Do NOT summarize, paraphrase, or skip any content\n\nAdditional instructions:\nBe brief."));
    }
}
