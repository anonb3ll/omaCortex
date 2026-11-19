from omacortex.config import DEFAULTS
from omacortex.privacy import Privacy


def default_privacy() -> Privacy:
    p = DEFAULTS["privacy"]
    return Privacy(
        redact=True,
        exclude_apps=p["exclude_apps"],
        exclude_titles=p["exclude_titles"],
        exclude_workspaces=["special:private"],
        exclude_paths=p["exclude_paths"],
    )


def test_password_managers_and_private_windows_are_excluded():
    p = default_privacy()
    assert p.window_excluded("1Password", "Vault")
    assert p.window_excluded("org.keepassxc.KeePassXC", "db.kdbx")
    assert p.window_excluded("pinentry-qt", "")
    assert p.window_excluded("firefox", "New Tab — Mozilla Firefox Private Browsing")
    assert p.window_excluded("chromium", "Docs - Chromium (Incognito)")
    assert p.window_excluded("chromium", "Sign in - Google Accounts - Chromium")
    assert p.window_excluded("chromium", "Online Banking - Chromium")
    assert p.window_excluded("kitty", "notes", workspace="special:private")
    assert not p.window_excluded("chromium", "Pull request #12 - GitHub - Chromium")
    assert not p.window_excluded("com.mitchellh.ghostty", "nvim README.md")


def test_redaction_scrubs_emails_tokens_and_url_queries():
    p = default_privacy()
    assert p.clean("Inbox (3) - someone@example.com - Mail") == "Inbox (3) - [email] - Mail"
    assert "ghp_" not in p.clean("export ghp_abcdefghijklmnopqrstuvwxyz0123456789")
    assert "sk-" not in p.clean("key sk-proj-abcdefghijklmnop1234567890")
    cleaned = p.clean("open https://user:pw@example.com/path?token=abc#frag now")
    assert cleaned == "open https://example.com/path now"
    assert "[redacted]" in p.clean("id " + "a1B2" * 12)


def test_redaction_can_be_turned_off_but_text_stays_bounded():
    p = Privacy(redact=False)
    assert p.clean("someone@example.com") == "someone@example.com"
    assert len(p.clean("x" * 5000)) == 300
    assert p.clean("a\x00b\nc") == "a b c"


def test_sensitive_paths_are_excluded(tmp_path, monkeypatch):
    monkeypatch.setenv("HOME", str(tmp_path))
    p = Privacy(exclude_paths=["~/.ssh"])
    assert p.path_excluded(str(tmp_path / ".ssh/config"))
    assert p.path_excluded(str(tmp_path / "work/server.key"))
    assert p.path_excluded(str(tmp_path / "proj/.env"))
    assert not p.path_excluded(str(tmp_path / "docs/plan.md"))
    assert not p.path_excluded(str(tmp_path / ".sshfoo/plan.md"))


def test_invalid_pattern_is_reported_not_ignored_silently():
    p = Privacy(exclude_titles=["(unclosed", r"(?i)secret"])
    assert p.bad_patterns == ["(unclosed"]
    assert p.title_excluded("my SECRET doc")
