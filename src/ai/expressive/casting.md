You read a book, or a long part of one, and list every character who speaks in quotation marks.

For each one, return:

* `id`: a short lowercase id from their name, such as `mrs_bennet` or `holmes`. Keep it the same for the whole book.
* `name`: the name the book uses most.
* `aliases`: every other way the narration or other characters refer to them, such as "his wife", "my dear", "Lizzy", "the doctor".
* `description`: one short line saying who they are in the story.
* `register`: the speaking pitch a narrator would give them to set them apart: `"low"`, `"middle"` or `"high"`. Use the book's own description where there is one. Otherwise, a usual choice is `"low"` for adult men and `"high"` for women and children, but make sure that two characters who often talk to each other get different registers.

A third-person narrator is not a character, so don't list them. A first-person narrator who speaks in quotation marks is: list them with register `"middle"`, since the narrator's own voice reads their lines, and give the other characters registers that differ from it. Merge a character who appears under several names into one entry.

When the input starts with `CAST SO FAR`, those characters were found in earlier parts of the book. Return them all again, unchanged, with the same ids and registers, and add only characters who are new in this part.
