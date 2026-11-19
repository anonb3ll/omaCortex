//! Configuration and on-disk locations.
//!
//! Everything lives under the XDG base directories of the current user:
//!
//! - config: `$XDG_CONFIG_HOME/omacortex/config.toml` (optional)
//! - data:   `$XDG_DATA_HOME/omacortex/cortex.db` (the activity store)
//! - state:  `$XDG_STATE_HOME/omacortex/` (consent, pause, daemon lock, control socket)
//!
//! The data and state directories are created 0700 and the database 0600.

use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capture {
    /// Focused window (app class + title) from Hyprland's event socket.
    pub windows: bool,
    /// What is playing, from MPRIS.
    pub media: bool,
    /// Files you open, from the freedesktop recently-used list.
    pub files: bool,
    /// Focus spans shorter than this are dropped (alt-tab noise).
    pub min_span_seconds: u64,
    /// Seconds without input before the shell plugin reports you as away.
    pub idle_seconds: u64,
    pub media_poll_seconds: u64,
    pub files_poll_seconds: u64,
    /// Pages reported by a browser extension through `omacortex browser install`.
    pub browser: bool,
    /// Page visits with less foreground time than this are dropped.
    pub min_page_seconds: u64,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            windows: true,
            media: true,
            files: true,
            min_span_seconds: 2,
            idle_seconds: 300,
            media_poll_seconds: 5,
            files_poll_seconds: 60,
            browser: true,
            min_page_seconds: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Privacy {
    /// Window focus history older than this is deleted automatically. 0 keeps everything.
    pub retention_days: u64,
    /// What you watched, read, opened and noted is kept this long, so you can
    /// still find a title months later. 0 keeps everything.
    pub library_days: u64,
    /// Replace e-mail addresses, tokens and URL query strings in titles.
    pub redact: bool,
    /// Window classes never recorded (case-insensitive, exact or glob).
    pub exclude_apps: Vec<String>,
    /// Window titles matching any of these regular expressions are never recorded.
    pub exclude_titles: Vec<String>,
    /// Workspaces never recorded, by name (for example "special:private").
    pub exclude_workspaces: Vec<String>,
    /// Files under these directories are never recorded.
    pub exclude_paths: Vec<String>,
    /// Web hosts never recorded (case-insensitive glob on the host name).
    pub exclude_hosts: Vec<String>,
    /// Query parameters kept in page addresses, as "host:param". Every other
    /// parameter is dropped. Without `v`, a YouTube address names no video.
    pub keep_url_params: Vec<String>,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

impl Default for Privacy {
    fn default() -> Self {
        Self {
            retention_days: 30,
            library_days: 365,
            redact: true,
            exclude_apps: strings(&[
                "1password",
                "bitwarden",
                "org.keepassxc.keepassxc",
                "keepassxc",
                "proton-pass",
                "proton pass",
                "*pinentry*",
                "*polkit*",
                "org.gnome.seahorse.application",
                "gcr-prompter",
            ]),
            exclude_titles: strings(&[
                r"(?i)\bincognito\b",
                r"(?i)private browsing",
                r"(?i)\bInPrivate\b",
                r"(?i)\bpassword\b",
                r"(?i)\b(sign|log) ?in\b",
                r"(?i)\bbank(ing)?\b",
                r"(?i)\b2fa\b|two-factor|authenticator",
            ]),
            exclude_workspaces: Vec::new(),
            exclude_paths: strings(&[
                "~/.ssh",
                "~/.gnupg",
                "~/.password-store",
                "~/.local/share/keyrings",
                "~/.config/omacortex",
            ]),
            exclude_hosts: strings(&[
                "accounts.*",
                "login.*",
                "signin.*",
                "auth.*",
                "sso.*",
                "id.*",
                "*.bank",
                "*bank*",
                "*paypal*",
                "*1password*",
                "*bitwarden*",
                "*lastpass*",
                "*okta*",
                "*auth0*",
            ]),
            keep_url_params: strings(&["youtube.com:v", "news.ycombinator.com:id"]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    /// "auto" probes local OpenAI-compatible servers; "off" disables; or a URL.
    pub endpoint: String,
    /// Empty picks the first model the server lists.
    pub model: String,
    /// Refuse any endpoint that is not on this machine unless true.
    pub allow_remote: bool,
    pub timeout_seconds: u64,
    pub max_tokens: u32,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            endpoint: "auto".into(),
            model: String::new(),
            allow_remote: false,
            timeout_seconds: 120,
            max_tokens: 600,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mcp {
    /// Let agents add notes and checkpoints to your timeline.
    pub allow_notes: bool,
    /// Hard cap on rows any single MCP call returns.
    pub max_results: usize,
}

impl Default for Mcp {
    fn default() -> Self {
        Self {
            allow_notes: true,
            max_results: 200,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub capture: Capture,
    pub privacy: Privacy,
    pub model: Model,
    pub mcp: Mcp,
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if Path::new(&v).is_absolute() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

impl Paths {
    pub fn from_env() -> Self {
        Self {
            config_dir: xdg("XDG_CONFIG_HOME", ".config").join("omacortex"),
            data_dir: xdg("XDG_DATA_HOME", ".local/share").join("omacortex"),
            state_dir: xdg("XDG_STATE_HOME", ".local/state").join("omacortex"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
    pub fn db(&self) -> PathBuf {
        self.data_dir.join("cortex.db")
    }
    pub fn consent_file(&self) -> PathBuf {
        self.state_dir.join("consent.json")
    }
    pub fn pause_file(&self) -> PathBuf {
        self.state_dir.join("pause.json")
    }
    pub fn lock_file(&self) -> PathBuf {
        self.state_dir.join("daemon.lock")
    }
    pub fn control_socket(&self) -> PathBuf {
        self.state_dir.join("control.sock")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [&self.data_dir, &self.state_dir] {
            fs::create_dir_all(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub paths: Paths,
    pub settings: Settings,
    /// A config file that failed to parse. Defaults stay in force: a broken
    /// file must never silently widen capture.
    pub error: String,
}

impl Config {
    pub fn load() -> Self {
        Self::load_from(Paths::from_env())
    }

    pub fn load_from(paths: Paths) -> Self {
        let mut cfg = Config {
            paths,
            settings: Settings::default(),
            error: String::new(),
        };
        let file = cfg.paths.config_file();
        if let Ok(text) = fs::read_to_string(&file) {
            match toml::from_str::<Settings>(&text) {
                Ok(settings) => cfg.settings = settings,
                Err(e) => cfg.error = format!("{}: {}", file.display(), e.message()),
            }
        }
        cfg
    }

    #[cfg(test)]
    pub fn with_settings(paths: Paths, settings: Settings) -> Self {
        Config {
            paths,
            settings,
            error: String::new(),
        }
    }
}

pub const EXAMPLE_CONFIG: &str = r#"# omaCortex configuration. Every key is optional; these are the defaults.
# Changes apply when the recorder restarts: `omarchy-shell shell reloadPlugins`
# or `systemctl --user restart omacortex`.

[capture]
windows = true            # focused window class + title (Hyprland)
media = true              # what is playing (MPRIS)
files = true              # files you open (recently-used.xbel)
min_span_seconds = 2      # drop focus spans shorter than this
idle_seconds = 300        # away after this long without input
media_poll_seconds = 5
files_poll_seconds = 60
browser = true            # pages from a browser extension (omacortex browser install)
min_page_seconds = 10     # drop page visits shorter than this

[privacy]
retention_days = 30       # window focus history; 0 keeps everything
library_days = 365        # what you watched, read, opened and noted; 0 keeps everything
redact = true             # scrub e-mails, tokens and URL queries from titles
exclude_apps = ["1password", "bitwarden", "org.keepassxc.keepassxc", "keepassxc", "proton-pass", "proton pass", "*pinentry*", "*polkit*", "org.gnome.seahorse.application", "gcr-prompter"]
exclude_titles = ['(?i)\bincognito\b', '(?i)private browsing', '(?i)\bInPrivate\b', '(?i)\bpassword\b', '(?i)\b(sign|log) ?in\b', '(?i)\bbank(ing)?\b', '(?i)\b2fa\b|two-factor|authenticator']
exclude_workspaces = []   # e.g. ["special:private"]
exclude_paths = ["~/.ssh", "~/.gnupg", "~/.password-store", "~/.local/share/keyrings", "~/.config/omacortex"]
exclude_hosts = ["accounts.*", "login.*", "signin.*", "auth.*", "sso.*", "id.*", "*.bank", "*bank*", "*paypal*", "*1password*", "*bitwarden*", "*lastpass*", "*okta*", "*auth0*"]
keep_url_params = ["youtube.com:v", "news.ycombinator.com:id"]   # "host:param" kept in page addresses

[model]
endpoint = "auto"         # "auto", "off", or e.g. "http://127.0.0.1:11434/v1"
model = ""                # empty = first model the server lists
allow_remote = false      # never send activity off this machine unless true
timeout_seconds = 120
max_tokens = 600

[mcp]
allow_notes = true        # agents may add notes/checkpoints
max_results = 200
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_is_exactly_the_defaults() {
        let parsed: Settings = toml::from_str(EXAMPLE_CONFIG).unwrap();
        assert_eq!(parsed, Settings::default());
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let parsed: Settings = toml::from_str("[capture]\nmedia = false\n").unwrap();
        assert!(!parsed.capture.media);
        assert!(parsed.capture.windows);
        assert_eq!(parsed.privacy, Privacy::default());
    }
}
