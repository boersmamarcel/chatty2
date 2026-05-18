//! Pure helpers used by `DaytonaTool` — file-extension checks,
//! Python-import parsing, and output-path detection.
//!
//! No I/O, no async. Kept separate so `mod.rs` is dominated by the
//! sandbox lifecycle and the rig-core `Tool` impl, not by string-munging
//! utilities and large static lookup tables.

use super::*;

pub(super) fn has_downloadable_extension(name: &str) -> bool {
    let lower = name.to_lowercase();
    DOWNLOADABLE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{}", ext)))
}

/// Common Python standard library modules that should NOT be pip-installed.
const PYTHON_STDLIB: &[&str] = &[
    "abc",
    "argparse",
    "ast",
    "asyncio",
    "base64",
    "bisect",
    "calendar",
    "cmath",
    "collections",
    "colorsys",
    "concurrent",
    "configparser",
    "contextlib",
    "copy",
    "csv",
    "ctypes",
    "dataclasses",
    "datetime",
    "decimal",
    "difflib",
    "email",
    "enum",
    "errno",
    "fcntl",
    "fileinput",
    "fnmatch",
    "fractions",
    "ftplib",
    "functools",
    "gc",
    "getpass",
    "glob",
    "gzip",
    "hashlib",
    "heapq",
    "hmac",
    "html",
    "http",
    "imaplib",
    "importlib",
    "inspect",
    "io",
    "ipaddress",
    "itertools",
    "json",
    "keyword",
    "linecache",
    "locale",
    "logging",
    "lzma",
    "math",
    "mimetypes",
    "multiprocessing",
    "numbers",
    "operator",
    "os",
    "pathlib",
    "pickle",
    "platform",
    "plistlib",
    "pprint",
    "pdb",
    "queue",
    "random",
    "re",
    "readline",
    "reprlib",
    "secrets",
    "select",
    "shelve",
    "shlex",
    "shutil",
    "signal",
    "site",
    "smtplib",
    "socket",
    "sqlite3",
    "ssl",
    "stat",
    "statistics",
    "string",
    "struct",
    "subprocess",
    "sys",
    "syslog",
    "tempfile",
    "textwrap",
    "threading",
    "time",
    "timeit",
    "tkinter",
    "token",
    "tokenize",
    "tomllib",
    "traceback",
    "tty",
    "turtle",
    "types",
    "typing",
    "unicodedata",
    "unittest",
    "urllib",
    "uuid",
    "venv",
    "warnings",
    "wave",
    "weakref",
    "webbrowser",
    "xml",
    "xmlrpc",
    "zipfile",
    "zipimport",
    "zlib",
    // Also exclude _ prefixed and __future__
    "__future__",
    "_thread",
];

/// Map import names to pip package names for common mismatches.
pub(super) fn pip_package_name(import_name: &str) -> &str {
    match import_name {
        "cv2" => "opencv-python",
        "PIL" => "Pillow",
        "sklearn" => "scikit-learn",
        "bs4" => "beautifulsoup4",
        "yaml" => "pyyaml",
        "attr" => "attrs",
        "dateutil" => "python-dateutil",
        "dotenv" => "python-dotenv",
        "gi" => "PyGObject",
        "lxml" => "lxml",
        "wx" => "wxPython",
        _ => import_name,
    }
}

/// Extract top-level Python import names from source code.
pub(super) fn extract_python_imports(code: &str) -> Vec<String> {
    let mut imports = std::collections::HashSet::new();
    for line in code.lines() {
        let trimmed = line.trim();
        // `import foo`, `import foo.bar`, `import foo as f`
        if let Some(rest) = trimmed.strip_prefix("import ") {
            for part in rest.split(',') {
                let module = part.split_whitespace().next().unwrap_or("");
                let top = module.split('.').next().unwrap_or("");
                if !top.is_empty() {
                    imports.insert(top.to_string());
                }
            }
        }
        // `from foo import bar`, `from foo.bar import baz`
        if let Some(rest) = trimmed.strip_prefix("from ") {
            let module = rest.split_whitespace().next().unwrap_or("");
            let top = module.split('.').next().unwrap_or("");
            if !top.is_empty() {
                imports.insert(top.to_string());
            }
        }
    }

    imports
        .into_iter()
        .filter(|name| !PYTHON_STDLIB.contains(&name.as_str()))
        .collect()
}

/// Extract file paths that the code writes to (e.g., savefig, write_html, to_csv).
///
/// Returns paths as written in the code — may be absolute or relative.
pub(super) fn extract_output_paths(code: &str) -> Vec<String> {
    use std::sync::LazyLock;

    static RE_SAVE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r#"(?:savefig|write_html|write_image|to_csv|to_excel|to_parquet|to_json|\.save)\s*\(\s*['"]([^'"]+)['"]"#
        ).unwrap()
    });
    static RE_OPEN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"open\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"][wa]"#).unwrap()
    });

    let mut paths = Vec::new();
    for cap in RE_SAVE.captures_iter(code) {
        if let Some(m) = cap.get(1) {
            paths.push(m.as_str().to_string());
        }
    }
    for cap in RE_OPEN.captures_iter(code) {
        if let Some(m) = cap.get(1) {
            let path = m.as_str();
            if has_downloadable_extension(path) {
                paths.push(path.to_string());
            }
        }
    }
    paths
}
