# InTome

InTome is an accessibility-first document reader for Windows, written in Rust. It is a port of the Python app Prism-Document-Reader. Its users are blind or visually impaired and use it through a screen reader, so what the app speaks is part of its behavior.

## Read first

- `README.md` lists build requirements and optional tools.
- `docs/PORTING.md` has the library mapping, the shared contracts, code conventions and the accessibility rules. Treat the accessibility rules as spec.
- The Python app at `C:\Users\khais\Documents\GitHub\Prism-Document-Reader` defines the behavior. Its `CLAUDE.md` gotchas for RichEdit and Gemini still apply here.

## Working here

- Before calling work done, `cargo build` has no new warnings and `cargo test --lib` passes.
- winsafe controls are not `Send`. Pass a control to a worker thread as its raw HWND (`hwnd().ptr() as isize`) and return to the UI thread with `events::run_on_ui`.
- A new setting is a field on a struct in `src/config.rs` with a default. `#[serde(default)]` lets older `intome.toml` files load without it.
- Per-document data (annotations, reading position) lives in `annotations.db`, keyed by `document::cache::file_hash`. The hash covers path, size and modification time, so a moved or edited file starts fresh.
- A user-visible change also updates `resources/user_guide.md`, which F1 opens.
- Log porting decisions and how you verified them as a row in `docs/decisions.tsv`.
- Commit subjects are imperative and sentence case, with no prefix, such as "Remember each document's reading position".
- Gemini model IDs come from settings, and Preferences lists the live ones. Never hardcode a guessed or preview model ID.

## Checking the UI

- `scripts/` holds the UI check tools. Each one describes itself in its header comment. `uia_audit.ps1` must pass for every window and dialog.
- `press_keys.ps1`, `value_change_probe.ps1` and the `nvda_*.ps1` scripts send real keystrokes to the foreground window. If the user is at the machine, the keys land in their other apps. Ask before running them.
- Don't change NVDA's settings or add-ons without asking.
- `tests/fixtures/` has a sample of every format. Undo any state a check leaves behind, such as a saved reading position.

## Releasing

- `/release` runs `scripts/release.ps1`. It bumps the version in `Cargo.toml`, tags `vX.Y.Z` with the commits since the last tag as its message, and pushes `main` and the tag.
- The tag push triggers `.github/workflows/release.yml`. It fails if the tag differs from the `Cargo.toml` version, builds with `scripts/package_release.ps1`, and publishes `InTome-X.Y.Z.zip` with the tag message as the release notes.
- Package from `vendor/` and `resources/`, never from `target\release`, which keeps stale DLLs from older builds.
- The in-app updater (`src/updater.rs`) is not live. Its manifest URL is a placeholder and it expects an Inno Setup installer, which the project doesn't build.

## Confirm before

- Committing, unless the user asked for it.
- Pushing to `main` outside `/release`, force-pushing, or deleting or moving a tag that was pushed.
- Running keystroke or NVDA scripts.

## Domain terms

- **Prism**: the Python app this repo ports. **prism** in lowercase, through the `prismer` crate, is the unrelated C++ speech library that InTome links statically for screen-reader output.
- **Page**: a unit of `Document`. Text positions are char indices into `Page::text`; only the GUI converts them to RichEdit offsets.
