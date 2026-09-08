//! Download safety: file-name policy and race-resistant final creation.
//!
//! File names arriving from HTTP responses are attacker-controlled.
//! [`DownloadPolicy`] produces a lexically contained candidate path;
//! [`SafeDownloadWriter`] enforces the filesystem boundary with an
//! exclusive temporary file and a non-overwriting final commit. Windows
//! additionally pins the ordinary download directory without delete
//! sharing and rejects paths redirected through reparse points.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Errors produced while validating a download target.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadPolicyError {
    /// The sanitized file name is empty.
    #[error("file name is empty after sanitization")]
    EmptyFileName,
    /// The resolved target path escapes the download directory.
    #[error("target path escapes the download directory")]
    PathEscape,
}

/// Errors produced while creating or committing a download file.
#[derive(Debug, thiserror::Error)]
pub enum SafeDownloadError {
    /// The configured directory is not a stable, ordinary directory.
    #[error("download directory is missing, not a directory, or is a reparse point")]
    UnsafeDirectory,
    /// The sanitized final component is too long for a portable download target.
    #[error("download file name is too long")]
    FileNameTooLong,
    /// The lexical download policy rejected the file name.
    #[error(transparent)]
    Policy(#[from] DownloadPolicyError),
    /// A filesystem operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Metadata returned only after a download has been published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadReceipt {
    path: PathBuf,
    bytes_written: u64,
    dangerous_type: bool,
}

impl DownloadReceipt {
    /// Final non-temporary path created for the download.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes accepted by the writer before the final sync.
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Whether the final name has an executable or script-like extension.
    pub fn is_dangerous_type(&self) -> bool {
        self.dangerous_type
    }
}

/// A download directory held open for the lifetime of all file creation.
///
/// On Windows, the directory handle denies delete sharing, rejects a final
/// reparse point and opens a canonical path that is rechecked after the handle
/// is acquired. This prevents an attacker from swapping the configured
/// directory or redirecting an ancestor junction between validation and
/// creation. Downloads are written to an exclusive temporary regular file,
/// synced, then published with a non-overwriting hard link in the same
/// directory while that file remains exclusively open. Existing files and
/// reparse points are therefore never followed or replaced.
#[derive(Debug)]
pub struct SafeDownloadWriter {
    canonical_dir: PathBuf,
    _directory_handle: File,
}

impl SafeDownloadWriter {
    /// Open and pin a trusted download directory.
    pub fn new(download_dir: impl AsRef<Path>) -> Result<Self, SafeDownloadError> {
        let (directory_handle, canonical_dir) = open_download_directory(download_dir.as_ref())?;
        Ok(Self {
            canonical_dir,
            _directory_handle: directory_handle,
        })
    }

    /// The stable, handle-resolved directory used for file operations.
    pub fn download_dir(&self) -> &Path {
        &self.canonical_dir
    }

    /// Create an exclusive temporary target for `file_name`.
    ///
    /// Dropping the returned target before [`SafeDownloadTarget::finish`]
    /// removes the temporary file. The final name is not visible until finish.
    pub fn create(&self, file_name: &str) -> Result<SafeDownloadTarget, SafeDownloadError> {
        let final_path = DownloadPolicy::new(&self.canonical_dir).target_path(file_name)?;
        let final_name = final_path
            .file_name()
            .ok_or(DownloadPolicyError::EmptyFileName)?;
        if final_name.to_string_lossy().encode_utf16().count() > 240 {
            return Err(SafeDownloadError::FileNameTooLong);
        }

        let dangerous_type = is_dangerous_extension(&final_path);
        let directory_handle = self._directory_handle.try_clone()?;
        for _ in 0..64 {
            let sequence = DOWNLOAD_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let temporary_name = format!(
                ".rust-browser-{}-{sequence}.download-part",
                std::process::id()
            );
            let temporary_path = self.canonical_dir.join(temporary_name);
            match open_new_download_file(&temporary_path) {
                Ok(file) => {
                    return Ok(SafeDownloadTarget {
                        file: Some(file),
                        _directory_handle: directory_handle,
                        temporary_path,
                        final_path,
                        bytes_written: 0,
                        dangerous_type,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate an exclusive temporary download file",
        )
        .into())
    }
}

static DOWNLOAD_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// An incomplete download that is deleted on drop unless it is committed.
#[derive(Debug)]
pub struct SafeDownloadTarget {
    file: Option<File>,
    _directory_handle: File,
    temporary_path: PathBuf,
    final_path: PathBuf,
    bytes_written: u64,
    dangerous_type: bool,
}

impl SafeDownloadTarget {
    /// The intended final path, which does not exist until [`Self::finish`].
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// Whether the sanitized final name is executable or script-like.
    pub fn is_dangerous_type(&self) -> bool {
        self.dangerous_type
    }

    /// Sync the temporary file and publish it without replacing any existing
    /// file or reparse point.
    pub fn finish(mut self) -> Result<DownloadReceipt, SafeDownloadError> {
        let mut file = self
            .file
            .take()
            .expect("download target always owns a file");
        file.flush()?;
        file.sync_all()?;
        // Creating a second directory entry is atomic and fails if the final
        // name already exists. The exclusive source handle remains live, so
        // another process cannot replace the temporary entry between sync and
        // publication. Both entries are inside the pinned directory.
        fs::hard_link(&self.temporary_path, &self.final_path)?;
        drop(file);
        let receipt = DownloadReceipt {
            path: self.final_path.clone(),
            bytes_written: self.bytes_written,
            dangerous_type: self.dangerous_type,
        };
        drop(self); // Drop removes only the private temporary name.
        Ok(receipt)
    }
}

impl Write for SafeDownloadTarget {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self
            .file
            .as_mut()
            .expect("download target always owns a file before finish")
            .write(buffer)?;
        self.bytes_written = self.bytes_written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file
            .as_mut()
            .expect("download target always owns a file before finish")
            .flush()
    }
}

impl Drop for SafeDownloadTarget {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.temporary_path);
    }
}

#[cfg(windows)]
fn open_download_directory(path: &Path) -> Result<(File, PathBuf), SafeDownloadError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let absolute = std::path::absolute(path)?;
    let canonical = fs::canonicalize(&absolute)?;
    if windows_path_key(&absolute) != windows_path_key(&canonical) {
        return Err(SafeDownloadError::UnsafeDirectory);
    }

    let handle = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&canonical)?;
    let metadata = handle.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(SafeDownloadError::UnsafeDirectory);
    }
    let canonical_after_open = fs::canonicalize(&absolute)?;
    if windows_path_key(&canonical) != windows_path_key(&canonical_after_open) {
        return Err(SafeDownloadError::UnsafeDirectory);
    }
    Ok((handle, canonical))
}

#[cfg(windows)]
fn windows_path_key(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('/', "\\");
    if let Some(rest) = value.strip_prefix(r"\\?\UNC\") {
        value = format!(r"\\{rest}");
    } else if let Some(rest) = value.strip_prefix(r"\\?\") {
        value = rest.to_string();
    }
    value.trim_end_matches('\\').to_lowercase()
}

#[cfg(windows)]
fn open_new_download_file(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(windows))]
fn open_download_directory(path: &Path) -> Result<(File, PathBuf), SafeDownloadError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(SafeDownloadError::UnsafeDirectory);
    }
    let handle = File::open(path)?;
    let canonical = fs::canonicalize(path)?;
    Ok((handle, canonical))
}

#[cfg(not(windows))]
fn open_new_download_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// Policy object for download path validation.
///
/// The target directory is fixed at construction time. All validation
/// is lexical (no filesystem I/O), which keeps the policy deterministic
/// and testable in memory. Lexical validation alone cannot prevent
/// TOCTOU attacks; use [`SafeDownloadWriter`] when bytes are written to
/// disk.
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
    /// cannot lexically escape it.
    pub fn target_path(&self, file_name: &str) -> Result<PathBuf, DownloadPolicyError> {
        let safe = sanitize_filename(file_name);
        if safe.is_empty() {
            return Err(DownloadPolicyError::EmptyFileName);
        }
        self.validate(&safe)
    }

    /// Validate that `relative_path` stays inside the download
    /// directory. No filesystem type check happens here.
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

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "rust-browser-download-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create isolated download test directory");
            // Hosted Windows runners may expose their temporary root through
            // a junction. Production correctly rejects such an unresolved
            // path, while these tests need an ordinary directory to exercise
            // the subsequent writer invariants.
            Self(fs::canonicalize(path).expect("canonicalize isolated test directory"))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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
        let root = PathBuf::from(r"C:\Users\alice\Downloads");
        let policy = DownloadPolicy::new(&root);
        let ok = policy.target_path("report.pdf").unwrap();
        assert_eq!(ok, root.join("report.pdf"));

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

    #[test]
    fn safe_writer_publishes_only_after_sync_and_finish() {
        let directory = TestDirectory::new("commit");
        let writer = SafeDownloadWriter::new(directory.path()).unwrap();
        let mut target = writer.create("report.txt").unwrap();
        let final_path = target.final_path().to_path_buf();
        assert!(!final_path.exists());

        target.write_all(b"complete download").unwrap();
        let receipt = target.finish().unwrap();

        assert_eq!(receipt.path(), final_path);
        assert_eq!(receipt.bytes_written(), 17);
        assert!(!receipt.is_dangerous_type());
        assert_eq!(fs::read(receipt.path()).unwrap(), b"complete download");
        assert_eq!(fs::read_dir(writer.download_dir()).unwrap().count(), 1);
    }

    #[test]
    fn dropped_or_failed_downloads_do_not_publish_partial_files() {
        let directory = TestDirectory::new("abort");
        let writer = SafeDownloadWriter::new(directory.path()).unwrap();
        let final_path;
        {
            let mut target = writer.create("partial.bin").unwrap();
            final_path = target.final_path().to_path_buf();
            target.write_all(b"partial").unwrap();
        }

        assert!(!final_path.exists());
        assert_eq!(fs::read_dir(writer.download_dir()).unwrap().count(), 0);
    }

    #[test]
    fn commit_never_replaces_an_existing_target() {
        let directory = TestDirectory::new("existing");
        let writer = SafeDownloadWriter::new(directory.path()).unwrap();
        let existing = writer.download_dir().join("report.txt");
        fs::write(&existing, b"original").unwrap();

        let mut target = writer.create("report.txt").unwrap();
        target.write_all(b"attacker controlled").unwrap();
        let error = target.finish().unwrap_err();

        assert!(matches!(error, SafeDownloadError::Io(_)));
        assert_eq!(fs::read(existing).unwrap(), b"original");
        assert_eq!(fs::read_dir(writer.download_dir()).unwrap().count(), 1);
    }

    #[test]
    fn safe_writer_bounds_names_and_reports_dangerous_types() {
        let directory = TestDirectory::new("metadata");
        let writer = SafeDownloadWriter::new(directory.path()).unwrap();
        assert!(matches!(
            writer.create(&format!("{}.txt", "x".repeat(241))),
            Err(SafeDownloadError::FileNameTooLong)
        ));

        let target = writer.create("invoice.pdf.exe").unwrap();
        assert!(target.is_dangerous_type());
    }

    #[cfg(windows)]
    #[test]
    fn pinned_directory_cannot_be_swapped_during_a_download() {
        let parent = TestDirectory::new("race");
        let downloads = parent.path().join("downloads");
        let moved = parent.path().join("downloads-moved");
        fs::create_dir(&downloads).unwrap();
        let writer = SafeDownloadWriter::new(&downloads).unwrap();

        assert!(fs::rename(&downloads, &moved).is_err());
        let mut target = writer.create("stable.txt").unwrap();
        target.write_all(b"stable").unwrap();
        let receipt = target.finish().unwrap();
        assert_eq!(fs::read(receipt.path()).unwrap(), b"stable");
    }

    #[cfg(windows)]
    #[test]
    fn active_target_keeps_the_directory_pinned_after_writer_drop() {
        let parent = TestDirectory::new("target-lifetime");
        let downloads = parent.path().join("downloads");
        let moved = parent.path().join("downloads-moved");
        fs::create_dir(&downloads).unwrap();
        let mut target = {
            let writer = SafeDownloadWriter::new(&downloads).unwrap();
            writer.create("stable.txt").unwrap()
        };

        assert!(fs::rename(&downloads, &moved).is_err());
        target.write_all(b"still pinned").unwrap();
        let receipt = target.finish().unwrap();
        assert_eq!(fs::read(receipt.path()).unwrap(), b"still pinned");
    }

    #[cfg(windows)]
    #[test]
    fn active_temporary_file_cannot_be_replaced_before_publication() {
        let directory = TestDirectory::new("exclusive-temp");
        let writer = SafeDownloadWriter::new(directory.path()).unwrap();
        let mut target = writer.create("stable.txt").unwrap();
        target.write_all(b"trusted bytes").unwrap();

        assert!(OpenOptions::new()
            .write(true)
            .open(&target.temporary_path)
            .is_err());
        assert!(fs::remove_file(&target.temporary_path).is_err());

        let receipt = target.finish().unwrap();
        assert_eq!(fs::read(receipt.path()).unwrap(), b"trusted bytes");
    }

    #[cfg(windows)]
    #[test]
    fn reparse_point_download_directory_is_rejected() {
        use std::process::{Command, Stdio};

        let parent = TestDirectory::new("reparse-root");
        let real = parent.path().join("real");
        let link = parent.path().join("link");
        fs::create_dir(&real).unwrap();
        let status = Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run the Windows junction fixture command");
        assert!(status.success(), "create directory junction fixture");

        assert!(matches!(
            SafeDownloadWriter::new(&link),
            Err(SafeDownloadError::UnsafeDirectory)
        ));
        fs::remove_dir(&link).unwrap();
    }
}
