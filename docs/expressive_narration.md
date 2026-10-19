# Expressive narration

Audio export (DAISY audio and M4B) can mark up the text before Gemini TTS reads it, so the narration has delivery and each character a voice of their own. The Export dialog offers **Off**, **Subtle** and **Dramatic**; the code is `src/ai/expressive/` and its use in `src/export/audio.rs`.

## Prompts and schemas

The prompts and JSON schemas are files beside the code, compiled in with `include_str!`:

| File | Use |
|---|---|
| `narration.md` | System instruction for marking up one passage (Subtle) |
| `dramatic.md` | Added after `narration.md` for Dramatic |
| `casting.md` | System instruction for the casting pass |
| `turns.schema.json` | Reply shape for a passage; `speaker` is filled at run time with `narrator`, `unknown` and the cast's ids |
| `cast.schema.json` | Reply shape for casting |

They started from a user-written prompt and were tuned with a throwaway trial script on *Pride and Prejudice* chapter 1 (third person, untagged back-and-forth) and *A Scandal in Bohemia* (first person), listened to by the user. What the trials settled:

- **A fixed list of 14 styles with the cues that trigger them**, enforced by the schema. Free-form styles swung between 0 and 11 styled lines per run on the same passage, with new wording every run; the list made the same lines get the same style every run. Older books needed literary cues ("cried", "exclaimed", an adverb after "said").
- **Narration is never styled and the narrator never makes a character's sounds.** The model often broke both, so code enforces them (checks below).
- **Speakers come from a cast.** Without one, the TTS voice lost track of who was talking in untagged dialogue. With the cast in the request and `speaker` limited to its ids, all 32 quotes of the *Pride and Prejudice* chapter went to the right character in every run, and *Holmes* 41 of 41.
- **Pitch, not identity, in the style.** Google's TTS guide allows pitch in `speech_metadata.style` but not age, gender, name or accent, so each character gets a `low`, `middle` or `high` register and the style sent is `"higher pitch, impatient"`. A first-person narrator is cast `middle`, so their lines stay in the narrator's own voice.
- **Context instead of recent speakers.** Chunks are made in parallel, so a passage can't wait for the previous one's speakers; it gets the two paragraphs before it instead, and attribution stayed at 100 %.
- **Defaults:** `gemini-3.5-flash-lite` (setting `gemini.narration_model`), default thinking (low to high thinking cost up to 6x for no gain), TTS stays `gemini-3.8-flash-lite-tts` (`gemini-3.8-flash-tts` sounded a little better but costs 1.5x). Expressive narration adds about 15-20 % to an export's Gemini cost.

## Pipeline

1. **Casting.** The whole book is cast in parts of up to 300,000 characters, each sent with the cast so far, which only grows: ids and registers stay fixed for the book. The cast is saved beside the audio parts (`%APPDATA%\InTome\cache\tts\<key>.json`), so a resumed export keeps the same characters and reuses the audio already made, and is deleted with them after a successful export.
2. **Markup per chunk.** Each ~3,000-character chunk is sent with the cast and its context. The reply is checked; a reply that fails is asked for once more, and a second failure reads that chunk plainly (logged), never with changed words.
3. **TTS.** Each turn becomes one text item with its style as a `speech_metadata` annotation.

A saved part's key includes the mode, the narration model and a hash of the cast, so plain and expressive audio, or audio from different casts, are never mixed.

## Checks on every reply

1. **Same words.** The turns' words, without tags, compared case-insensitively and ignoring punctuation, must equal the passage's. A turn boundary counts as a word boundary, since turns are separate TTS items.
2. **Known tags only.** Tags outside Google's list (minus `<whispers>`, since whispering is a style) are removed. A vocal tag outside quotes stays only right after a closing quote or in a first-person sentence ("I laughed. <laugh>"); pauses may go anywhere.
3. **Known styles and speakers.** A style outside the list is cleared; a speaker outside the cast becomes `unknown`.
4. **Narration stays plain.** Every turn is split at its quotation marks; narration pieces go to the unstyled narrator, and a quote given to the narrator becomes `unknown`.
5. **Interrupted quotes.** A quote broken by a short speech tag in the same paragraph ("They want you to leave," I said quietly. "The council...") is one utterance: an `unknown` second half takes the first half's speaker, an unstyled one its style.
