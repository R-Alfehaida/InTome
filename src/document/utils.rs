//! STUB: port of prism/document/utils.py. Signatures are the contract.

/// Splits `text` at paragraph boundaries into pages of about `page_size` chars.
pub fn paginate_text(text: &str, _page_size: usize) -> Vec<String> {
    vec![text.to_string()]
}

/// True when an external tool (pandoc, antiword, tesseract, ffmpeg) is on PATH.
pub fn check_external_tool(_name: &str) -> bool {
    false
}

/// Runs an external tool with no console window and returns stdout.
pub fn run_external_tool(name: &str, _args: &[&str], _timeout_secs: u64) -> anyhow::Result<Vec<u8>> {
    anyhow::bail!("{name} not available")
}
