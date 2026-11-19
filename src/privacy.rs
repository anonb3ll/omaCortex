//! What is never recorded, and how recorded text is scrubbed.
//!
//! The guard runs before anything touches the database. An excluded window,
//! workspace or path is dropped entirely (the timeline shows a gap), and every
//! stored string passes through [`Privacy::clean`].

use crate::config;
use regex_lite::{Captures, Regex};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

pub const MAX_TEXT: usize = 300;

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[a-zA-Z][a-zA-Z0-9+.-]*://[^\s]+").unwrap());
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap());
// Well-known credential shapes first, then any long opaque run of characters.
static TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}",
        r"|xox[abposr]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,}",
        r"|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,})",
        r"|\b[A-Za-z0-9_-]{40,}\b"
    ))
    .unwrap()
});

/// Replace control characters, trim, and cap the length in characters.
pub fn bounded(text: &str, limit: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= limit {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(limit - 1).collect();
        out.push('…');
        out
    }
}

/// Keep scheme, host, port and path; drop credentials, query and fragment.
fn strip_url(raw: &str) -> String {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return raw.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let after = &rest[end..];
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    format!("{scheme}://{host}{}", &after[..path_end])
}

/// Case-insensitive glob with `*` and `?`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

pub fn expand_home(path: &str) -> PathBuf {
    if path == "~" {
        config::home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        config::home().join(rest)
    } else {
        PathBuf::from(path)
    }
}

/// Resolve symlinks where the path exists; otherwise normalise lexically.
fn real(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct Privacy {
    pub redact: bool,
    exclude_apps: Vec<String>,
    exclude_titles: Vec<Regex>,
    /// Patterns that failed to compile, reported by `omacortex doctor`.
    pub bad_patterns: Vec<String>,
    exclude_workspaces: HashSet<String>,
    exclude_paths: Vec<PathBuf>,
}

impl Privacy {
    pub fn new(
        redact: bool,
        exclude_apps: &[String],
        exclude_titles: &[String],
        exclude_workspaces: &[String],
        exclude_paths: &[String],
    ) -> Self {
        let mut titles = Vec::new();
        let mut bad = Vec::new();
        for pattern in exclude_titles {
            match Regex::new(pattern) {
                Ok(re) => titles.push(re),
                Err(_) => bad.push(pattern.clone()),
            }
        }
        Self {
            redact,
            exclude_apps: exclude_apps
                .iter()
                .filter(|a| !a.is_empty())
                .map(|a| a.to_lowercase())
                .collect(),
            exclude_titles: titles,
            bad_patterns: bad,
            exclude_workspaces: exclude_workspaces
                .iter()
                .filter(|w| !w.is_empty())
                .cloned()
                .collect(),
            exclude_paths: exclude_paths
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| real(&expand_home(p)))
                .collect(),
        }
    }

    pub fn from_settings(p: &config::Privacy) -> Self {
        Self::new(
            p.redact,
            &p.exclude_apps,
            &p.exclude_titles,
            &p.exclude_workspaces,
            &p.exclude_paths,
        )
    }

    pub fn app_excluded(&self, app: &str) -> bool {
        self.exclude_apps.iter().any(|p| glob_match(p, app))
    }

    pub fn title_excluded(&self, title: &str) -> bool {
        self.exclude_titles.iter().any(|re| re.is_match(title))
    }

    pub fn window_excluded(&self, app: &str, title: &str, workspace: &str) -> bool {
        (!workspace.is_empty() && self.exclude_workspaces.contains(workspace))
            || self.app_excluded(app)
            || self.title_excluded(title)
    }

    pub fn path_excluded(&self, path: &str) -> bool {
        let r = real(&expand_home(path));
        if self.exclude_paths.iter().any(|root| r.starts_with(root)) {
            return true;
        }
        let name = r
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        [".key", ".pem", ".kdbx", ".gpg", ".asc"]
            .iter()
            .any(|ext| name.ends_with(ext))
            || [".env", "id_rsa", "id_ed25519", ".netrc"].contains(&name.as_str())
    }

    pub fn clean(&self, text: &str) -> String {
        let mut text = bounded(text, 2000);
        if self.redact && !text.is_empty() {
            text = URL
                .replace_all(&text, |c: &Captures| strip_url(&c[0]))
                .into_owned();
            text = EMAIL.replace_all(&text, "[email]").into_owned();
            text = TOKENS.replace_all(&text, "[redacted]").into_owned();
        }
        bounded(&text, MAX_TEXT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Privacy {
        let p = config::Privacy::default();
        Privacy::new(
            true,
            &p.exclude_apps,
            &p.exclude_titles,
            &["special:private".into()],
            &p.exclude_paths,
        )
    }

    #[test]
    fn password_managers_and_private_windows_are_excluded() {
        let p = defaults();
        assert!(p.window_excluded("1Password", "Vault", ""));
        assert!(p.window_excluded("org.keepassxc.KeePassXC", "db.kdbx", ""));
        assert!(p.window_excluded("pinentry-qt", "", ""));
        assert!(p.window_excluded("firefox", "New Tab — Mozilla Firefox Private Browsing", ""));
        assert!(p.window_excluded("chromium", "Docs - Chromium (Incognito)", ""));
        assert!(p.window_excluded("chromium", "Sign in - Google Accounts - Chromium", ""));
        assert!(p.window_excluded("chromium", "Online Banking - Chromium", ""));
        assert!(p.window_excluded("kitty", "notes", "special:private"));
        assert!(!p.window_excluded("chromium", "Pull request #12 - GitHub - Chromium", ""));
        assert!(!p.window_excluded("com.mitchellh.ghostty", "nvim README.md", ""));
    }

    #[test]
    fn redaction_scrubs_emails_tokens_and_url_queries() {
        let p = defaults();
        assert_eq!(
            p.clean("Inbox (3) - someone@example.com - Mail"),
            "Inbox (3) - [email] - Mail"
        );
        assert!(
            !p.clean("export ghp_abcdefghijklmnopqrstuvwxyz0123456789")
                .contains("ghp_")
        );
        assert!(
            !p.clean("key sk-proj-abcdefghijklmnop1234567890")
                .contains("sk-")
        );
        assert_eq!(
            p.clean("open https://user:pw@example.com/path?token=abc#frag now"),
            "open https://example.com/path now"
        );
        assert!(
            p.clean(&format!("id {}", "a1B2".repeat(12)))
                .contains("[redacted]")
        );
    }

    #[test]
    fn redaction_can_be_off_but_text_stays_bounded() {
        let p = Privacy::new(false, &[], &[], &[], &[]);
        assert_eq!(p.clean("someone@example.com"), "someone@example.com");
        assert_eq!(p.clean(&"x".repeat(5000)).chars().count(), 300);
        assert_eq!(p.clean("a\0b\nc"), "a b c");
    }

    #[test]
    fn sensitive_paths_are_excluded() {
        let p = Privacy::new(true, &[], &[], &[], &["/tmp/oc-test-root/.ssh".into()]);
        assert!(p.path_excluded("/tmp/oc-test-root/.ssh/config"));
        assert!(p.path_excluded("/tmp/oc-test-root/work/server.key"));
        assert!(p.path_excluded("/tmp/oc-test-root/proj/.env"));
        assert!(!p.path_excluded("/tmp/oc-test-root/docs/plan.md"));
        assert!(!p.path_excluded("/tmp/oc-test-root/.sshfoo/plan.md"));
        assert!(p.path_excluded("/tmp/oc-test-root/x/../.ssh/id"));
    }

    #[test]
    fn invalid_patterns_are_reported() {
        let p = Privacy::new(
            true,
            &[],
            &["(unclosed".into(), "(?i)secret".into()],
            &[],
            &[],
        );
        assert_eq!(p.bad_patterns, vec!["(unclosed".to_string()]);
        assert!(p.title_excluded("my SECRET doc"));
    }

    #[test]
    fn globs() {
        assert!(glob_match("*pinentry*", "Pinentry-Qt"));
        assert!(glob_match("1password", "1Password"));
        assert!(!glob_match("1password", "1password-helper"));
        assert!(glob_match("a?c", "abc"));
    }
}
