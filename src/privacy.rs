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

/// The host of an http(s) address, lower-cased, without credentials or port.
pub fn host_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return None;
    }
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split_once(']').map_or(host, |(h, _)| h)
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches('.')
        .to_lowercase();
    (!host.is_empty()).then_some(host)
}

/// A path segment that looks like an identifier or secret rather than words:
/// a long unbroken run, as in a share link or a session path.
fn opaque_segment(segment: &str) -> bool {
    segment.split(['-', '_', '.', '~', '+']).any(|piece| {
        piece.chars().count() >= 24 && piece.chars().all(|c| c.is_ascii_alphanumeric())
    })
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
    exclude_hosts: Vec<String>,
    /// (host suffix, parameter) pairs that survive [`Privacy::clean_url`].
    keep_url_params: Vec<(String, String)>,
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
            exclude_hosts: Vec::new(),
            keep_url_params: Vec::new(),
        }
    }

    /// Rules for web pages: hosts never recorded and the query parameters kept.
    pub fn with_web(mut self, exclude_hosts: &[String], keep_url_params: &[String]) -> Self {
        self.exclude_hosts = exclude_hosts
            .iter()
            .filter(|h| !h.is_empty())
            .map(|h| h.to_lowercase())
            .collect();
        self.keep_url_params = keep_url_params
            .iter()
            .filter_map(|rule| rule.split_once(':'))
            .filter(|(h, p)| !h.is_empty() && !p.is_empty())
            .map(|(h, p)| (h.to_lowercase(), p.to_string()))
            .collect();
        self
    }

    pub fn from_settings(p: &config::Privacy) -> Self {
        Self::new(
            p.redact,
            &p.exclude_apps,
            &p.exclude_titles,
            &p.exclude_workspaces,
            &p.exclude_paths,
        )
        .with_web(&p.exclude_hosts, &p.keep_url_params)
    }

    pub fn host_excluded(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        let bare = host.strip_prefix("www.").unwrap_or(&host);
        self.exclude_hosts
            .iter()
            .any(|p| glob_match(p, &host) || glob_match(p, bare))
    }

    /// A page address safe to store: http(s) only, no credentials, no
    /// fragment, and no query string apart from the parameters listed in
    /// `keep_url_params` for that host. Path segments that look like tokens
    /// are replaced. Returns None for anything that is not a web page.
    pub fn clean_url(&self, raw: &str) -> Option<String> {
        let raw = bounded(raw, 2000);
        let host = host_of(&raw)?;
        let (scheme, rest) = raw.split_once("://")?;
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        // Keep the port: localhost:3000 and localhost:8080 are different places.
        let authority = rest[..authority_end]
            .rsplit_once('@')
            .map_or(&rest[..authority_end], |(_, h)| h)
            .to_lowercase();
        let after = &rest[authority_end..];
        let path_end = after.find(['?', '#']).unwrap_or(after.len());
        let query = after[path_end..]
            .strip_prefix('?')
            .map(|q| &q[..q.find('#').unwrap_or(q.len())])
            .unwrap_or("");
        let path: Vec<String> = after[..path_end]
            .split('/')
            .map(|seg| {
                if self.redact && opaque_segment(seg) {
                    "[redacted]".to_string()
                } else {
                    seg.to_string()
                }
            })
            .collect();
        let bare = host.strip_prefix("www.").unwrap_or(&host);
        let kept: Vec<String> = query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .filter(|(name, value)| {
                !value.is_empty()
                    && value.len() <= 64
                    && self
                        .keep_url_params
                        .iter()
                        .any(|(h, p)| p == name && (bare == h || bare.ends_with(&format!(".{h}"))))
            })
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let mut out = format!(
            "{}://{authority}{}",
            scheme.to_ascii_lowercase(),
            path.join("/")
        );
        if !kept.is_empty() {
            out.push('?');
            out.push_str(&kept.join("&"));
        }
        if self.redact {
            out = EMAIL
                .replace_all(&out.replace("%40", "@"), "[email]")
                .into_owned();
        }
        Some(bounded(&out, MAX_TEXT))
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
    fn page_addresses_lose_queries_tokens_and_credentials() {
        let p = Privacy::from_settings(&config::Privacy::default());
        assert_eq!(
            p.clean_url("https://user:pw@Example.com:8443/a/b?utm=1&q=secret#frag")
                .as_deref(),
            Some("https://example.com:8443/a/b")
        );
        // The video id is what makes a YouTube address worth keeping.
        assert_eq!(
            p.clean_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=42s&list=PLx")
                .as_deref(),
            Some("https://www.youtube.com/watch?v=dQw4w9WgXcQ")
        );
        assert_eq!(
            p.clean_url("https://evil.example/watch?v=dQw4w9WgXcQ")
                .as_deref(),
            Some("https://evil.example/watch")
        );
        // Long slugs made of words survive; opaque identifiers do not.
        let slug =
            "https://writer.substack.com/p/why-small-models-will-win-the-next-decade-of-computing";
        assert_eq!(p.clean_url(slug).as_deref(), Some(slug));
        assert_eq!(
            p.clean_url("https://files.example/share/9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c/report")
                .as_deref(),
            Some("https://files.example/share/[redacted]/report")
        );
        assert_eq!(
            p.clean_url("https://mail.example/u/someone%40example.com/inbox")
                .as_deref(),
            Some("https://mail.example/u/[email]/inbox")
        );
        for not_a_page in [
            "chrome://settings",
            "file:///home/u/x.html",
            "about:blank",
            "notaurl",
            "",
        ] {
            assert_eq!(p.clean_url(not_a_page), None);
        }
    }

    #[test]
    fn sensitive_hosts_are_excluded() {
        let p = Privacy::from_settings(&config::Privacy::default());
        for host in [
            "accounts.google.com",
            "login.microsoftonline.com",
            "www.chase.bank",
            "www.bankofamerica.com",
            "www.paypal.com",
            "vault.bitwarden.com",
            "dev-1234.okta.com",
        ] {
            assert!(p.host_excluded(host), "{host}");
        }
        for host in [
            "www.youtube.com",
            "news.ycombinator.com",
            "writer.substack.com",
            "github.com",
        ] {
            assert!(!p.host_excluded(host), "{host}");
        }
        assert_eq!(
            host_of("https://User@Sub.Example.com:8080/x").as_deref(),
            Some("sub.example.com")
        );
        assert_eq!(host_of("http://[::1]:3000/").as_deref(), Some("::1"));
        assert_eq!(host_of("ftp://example.com/"), None);
    }

    #[test]
    fn globs() {
        assert!(glob_match("*pinentry*", "Pinentry-Qt"));
        assert!(glob_match("1password", "1Password"));
        assert!(!glob_match("1password", "1password-helper"));
        assert!(glob_match("a?c", "abc"));
    }
}
