//! Download safety: file-name sanitization and path validation.
//!
//! File names arriving from HTTP responses are attacker-controlled.
//! This module guarantees that a download target path always stays
//! inside the configured download directory, and that file names can
//! never escape it via `..`, absolute paths, Windows drive letters,
//! reserved device names or hidden alternates.

use std::path::{Component, Path, PathBuf};

/// Errors produced while validating a download target.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadPolicyError {
    /// The sanitized file name is empty.
    #[error("file name is empty after sanitization")]
    EmptyFileName,
    /// The resolved target path escapes the download directory.
    #[error("target path escapes the download directory")]
    PathEscape,
    /// The resolved target is not a plain file (e.g. a directory).
    #[error("target path is not a regular file")]
    NotAFile,
}

/// Policy object for download path validation.
///
/// The target directory is fixed at construction time. All validation
/// is lexical (no filesystem I/O): this avoids TOCTOU races between
/// check and use and keeps the policy testable in-memory.
#[derive(Debug, Clone)]
pub struct DownloadPolicy {
    download_dir: PathBuf,
}

impl DownloadPolicy {
    /// Create a policy rooted at `download_dir`.
    pub fn new(download_dir: impl Into<PathBuf>) -> Self {
        Self {
            download_dir: download_dir.into(),
        }
    }

    /// The configured download directory.
    pub fn download_dir(&self) -> &Path {
        &self.download_dir
    }

    /// Sanitize `file_name` (attacker-controlled, from a response
    /// header) and validate that placing it in the download directory
    /// cannot escape it.
    pub fn target_path(&self, file_name: &str) -> Result<PathBuf, DownloadPolicyError> {
        let safe = sanitize_filename(file_name);
        if safe.is_empty() {
            return Err(DownloadPolicyError::EmptyFileName);
        }
        self.validate(&safe)
    }

    /// Validate that `relative_path` stays inside the download
    /// directory and refers to a plain file.
    fn validate(&self, relative_path: &str) -> Result<PathBuf, DownloadPolicyError> {
        let joined = self.download_dir.join(relative_path);
        let root = self.download_dir.as_path();

        // Lexically normalize and verify containment.
        let mut components = joined.components().peekable();
        for component in &mut components {
            match component {
                Component::Normal(_) => (),
                // `.` is a no-op.
                Component::CurDir => (),
                // `..` escaping the root is forbidden.
                Component::ParentDir => return Err(DownloadPolicyError::PathEscape),
                Component::RootDir | Component::Prefix(_) => {
                    // Reached the filesystem root while the target is not
                    // the root itself; if we are still inside `root`, the
                    // remaining components are fine, otherwise the path
                    // escaped upward.
                    if !joined.starts_with(root) {
                        return Err(DownloadPolicyError::PathEscape);
                    }
                }
            }
        }
        if !joined.starts_with(root) {
            return Err(DownloadPolicyError::PathEscape);
        }
        Ok(joined)
    }
}

/// Characters that must never appear in a download file name.
const FORBIDDEN_CHARS: &[char] = &[
    '/', '\\', '\0', ':', '*', '?', '"', '<', '>', '|', '\r', '\n',
];

/// Windows reserved device names (case-insensitive), with or without
/// extension. Writing to these names addresses devices, not files.
const RESERVED_DEVICE_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Sanitize an attacker-controlled file name.
///
/// - Replaces path separators and Windows-forbidden characters.
/// - Rejects empty and dot-only names.
/// - Rewrites Windows reserved device names.
/// - Strips surrounding whitespace and control characters.
/// - Never returns a path component that can traverse (`..`, `.`).
pub fn sanitize_filename(file_name: &str) -> String {
    // Trim whitespace and control characters.
    let trimmed: String = file_name
        .trim_matches(|c: char| c.is_whitespace() || c.is_control())
        .chars()
        .collect();

    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return String::new();
    }

    // Replace forbidden characters. On Windows, both `/` and `\` are
    // separators; elsewhere `\` is a legal file-name character, but we
    // replace it everywhere for consistent, portable behavior.
    let replaced: String = trimmed
        .chars()
        .map(|c| if FORBIDDEN_CHARS.contains(&c) { '_' } else { c })
        .collect();

    // Guard against Windows device names ("CON", "CON.txt", "con.log").
    let stem = replaced
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if RESERVED_DEVICE_NAMES.contains(&stem.as_str()) {
        return format!("_{replaced}");
    }

    // Trailing dots/spaces are stripped by Windows and can confuse
    // extension handling; replace them instead of stripping, so the
    // visible name matches the on-disk name.
    let mut result: String = replaced.trim_end_matches([' ', '.']).chars().collect();
    if result.is_empty() {
        result = "download".to_string();
    }
    result
}

/// True if the extension (or double extension) is in the dangerous set:
/// executables and script formats that must be flagged or refused when
/// opened directly after download.
pub fn is_dangerous_extension(path: &Path) -> bool {
    let name = match path.file_name() {
        Some(name) => name.to_string_lossy().to_ascii_lowercase(),
        None => return false,
    };
    const DANGEROUS: &[&str] = &[
        "exe", "bat", "cmd", "com", "scr", "pif", "msi", "msp", "mst", "js", "jse", "vbs", "vbe",
        "wsf", "wsh", "ps1", "psm1", "sh", "lnk", "app", "hta", "reg", "gadget", "jar", "msc",
        "cpl", "dll", "sys", "drv", "html", "htm",
    ];
    let Some(ext) = path.extension() else {
        return false;
    };
    let ext = ext.to_string_lossy().to_ascii_lowercase();

    // Double-extension heuristic: "report.pdf.exe" must be caught.
    let components: Vec<&str> = name.split('.').collect();
    let mut trailing = components
        .iter()
        .rev()
        .take(2)
        .filter_map(|c| (!c.is_empty()).then_some(*c));
    trailing.any(|c| DANGEROUS.contains(&c)) || DANGEROUS.contains(&ext.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_path_traversal() {
        assert_eq!(sanitize_filename("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize_filename("..\\..\\win.ini"), ".._.._win.ini");
        assert_eq!(
            sanitize_filename("a/b\\c:d*e?f\"g<h>i|j"),
            "a_b_c_d_e_f_g_h_i_j"
        );
    }

    #[test]
    fn sanitizes_forbidden_and_control() {
        // Each forbidden character is replaced individually.
        assert_eq!(sanitize_filename("evil\r\nname.txt"), "evil__name.txt");
        assert_eq!(sanitize_filename("  spaced.txt  "), "spaced.txt");
        assert_eq!(sanitize_filename("name\0.bin"), "name_.bin");
    }

    #[test]
    fn rejects_empty_and_dot_names() {
        assert_eq!(sanitize_filename(""), "");
        assert_eq!(sanitize_filename("."), "");
        assert_eq!(sanitize_filename(".."), "");
        assert_eq!(sanitize_filename("   "), "");
    }

    #[test]
    fn rewrites_reserved_device_names() {
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con.log"), "_con.log");
        assert_eq!(sanitize_filename("NUL"), "_NUL");
        assert_eq!(sanitize_filename("COM1.txt"), "_COM1.txt");
        assert_eq!(sanitize_filename("LPT9"), "_LPT9");
        // Not reserved, must stay untouched.
        assert_eq!(sanitize_filename("console.txt"), "console.txt");
        assert_eq!(sanitize_filename("com10.txt"), "com10.txt");
    }

    #[test]
    fn handles_unicode_and_legal_names() {
        assert_eq!(sanitize_filename("görüntü.png"), "görüntü.png");
        assert_eq!(sanitize_filename("photo (1).jpg"), "photo (1).jpg");
        assert_eq!(sanitize_filename("a.b.c.tar.gz"), "a.b.c.tar.gz");
    }

    #[test]
    fn trailing_dots_and_spaces_normalized() {
        assert_eq!(sanitize_filename("file.txt "), "file.txt");
        assert_eq!(sanitize_filename("file..."), "file");
        assert_eq!(sanitize_filename("..."), "download");
    }

    #[test]
    fn target_path_stays_in_directory() {
        let policy = DownloadPolicy::new(r"C:\Users\alice\Downloads");
        let ok = policy.target_path("report.pdf").unwrap();
        assert_eq!(ok, PathBuf::from(r"C:\Users\alice\Downloads\report.pdf"));

        // Traversal is neutralized by sanitization before validation.
        let safe = policy.target_path("../../etc/passwd").unwrap();
        assert!(safe.starts_with(policy.download_dir()));

        // Absolute path in the name cannot escape either.
        let safe = policy.target_path(r"C:\Windows\evil.exe").unwrap();
        assert!(safe.starts_with(policy.download_dir()));
        assert_eq!(safe.file_name().unwrap(), "C__Windows_evil.exe");
    }

    #[test]
    fn empty_name_is_rejected() {
        let policy = DownloadPolicy::new(r"C:\Users\alice\Downloads");
        assert_eq!(
            policy.target_path(".."),
            Err(DownloadPolicyError::EmptyFileName)
        );
        assert_eq!(
            policy.target_path(""),
            Err(DownloadPolicyError::EmptyFileName)
        );
    }

    #[test]
    fn dangerous_extensions_detected() {
        assert!(is_dangerous_extension(Path::new("setup.exe")));
        assert!(is_dangerous_extension(Path::new("invoice.pdf.exe")));
        assert!(is_dangerous_extension(Path::new("script.js")));
        assert!(is_dangerous_extension(Path::new("page.html")));
        assert!(!is_dangerous_extension(Path::new("report.pdf")));
        assert!(!is_dangerous_extension(Path::new("image.png")));
        assert!(!is_dangerous_extension(Path::new("archive.tar.gz")));
    }
}
