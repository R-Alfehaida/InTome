use std::path::{Path, PathBuf};

fn main() {
    // comctl32 v6 gives themed common controls and their full accessibility providers.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'");

    // Copy runtime assets (pdfium.dll, screen-reader DLLs, user guide) next to the exe.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let profile_dir = out.ancestors().nth(3).unwrap().to_path_buf();
    for dir in ["vendor", "resources"] {
        println!("cargo:rerun-if-changed={dir}");
        copy_dir(Path::new(dir), &profile_dir);
    }
}

fn copy_dir(src: &Path, dst: &Path) {
    let Ok(entries) = std::fs::read_dir(src) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() {
            let _ = std::fs::copy(&p, dst.join(e.file_name()));
        }
    }
}
