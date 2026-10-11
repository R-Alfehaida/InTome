# InTome

InTome is a document reader for blind and visually impaired users, used through a screen reader. What the app speaks is part of its behaviour.

## Language

**Document**:
An open file as InTome reads it: an ordered list of Pages.
_Avoid_: file, book

**Page**:
One unit of a Document, shown alone in the reading area. Positions in a Page count characters of its text.
_Avoid_: screen, chunk

**Reading position**:
Where the user is in a Document: one Page and one character in it. InTome saves it per Document and restores it on reopen.
_Avoid_: cursor, caret (for the concept), bookmark, progress

**Move**:
A change of Reading position that the user or the app asks for, such as next Page, a heading jump or a slider drag. Each Move decides what InTome speaks.
_Avoid_: jump, navigation (for one change)

**Continuous reading**:
The screen reader's say-all flowing across Pages: when the caret reaches the end of a Page, InTome turns to the next one without speaking.
_Avoid_: auto-advance, say-all (for InTome's part)

**Read-aloud**:
InTome reading the Document with its own TTS voice, apart from the screen reader.
_Avoid_: TTS reading, continuous reading
