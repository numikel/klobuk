//! Scrubbing of text that goes into the committable export (`.factory/issues.jsonl`).
//!
//! Two pure functions, no IO and no environment: [`redact_home_paths`] rewrites a path under a home
//! directory so the account name is gone (`C:\Users\anna\src\a.rs` becomes `~\src\a.rs`), and
//! [`scan_findings`] names the kinds of e-mail address and secret-like string a text holds. A finding
//! never carries the matched value.

use std::borrow::Cow;

// ---- home paths ----

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

/// Characters of an account name: `anna`, `john.doe`, `RUNNER~1` (the 8.3 short form), `Zoë`.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '~')
}

/// Whether the byte at `i` continues a name (a lone `.` before a non-name byte ends a sentence).
fn continues_name(b: &[u8], i: usize) -> bool {
    match b.get(i) {
        None => false,
        Some(c) if c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'~') || *c >= 0x80 => true,
        Some(b'.') => b
            .get(i + 1)
            .is_some_and(|n| n.is_ascii_alphanumeric() || matches!(n, b'_' | b'-' | b'~') || *n >= 0x80),
        Some(_) => false,
    }
}

/// Byte length of the account name at the start of `rest`, or `None` when there is none. With
/// `sep` (the separator that preceded the name), `Max Muster` is one name when that same separator
/// closes it, it has at most two spaces and every word after the first starts with a capital or a
/// digit. Otherwise the name ends at the first space, so prose after a path
/// (`C:\Users\anna and src/lib.rs`) is never swallowed. Without `sep` it always ends at the first space.
fn name_len(rest: &str, sep: Option<char>) -> Option<usize> {
    let mut len = rest.find(|c: char| !is_name_char(c)).unwrap_or(rest.len());
    if let Some(sep) = sep {
        let seg = rest
            .find(|c: char| !(is_name_char(c) || c == ' '))
            .unwrap_or(rest.len());
        let name = &rest[..seg];
        let words_ok = name
            .split(' ')
            .skip(1)
            .all(|w| w.chars().next().is_some_and(|c| !c.is_lowercase()));
        if rest[seg..].starts_with(sep)
            && name.matches(' ').count() <= 2
            && !name.starts_with(' ')
            && words_ok
        {
            len = seg;
        }
    }
    let name = rest[..len].trim_end_matches('.');
    (!name.is_empty()).then_some(name.len())
}

fn leading_seps(s: &str) -> usize {
    s.len() - s.trim_start_matches(is_sep).len()
}

/// `X:\Users\<name>` or `X:/Users/<name>` (any drive, either slash, `\\` runs, any case of `Users`).
fn windows_home(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    let seps = leading_seps(&s[2..]);
    let after = &s[2 + seps..];
    if seps == 0 || !after.get(..5)?.eq_ignore_ascii_case("users") {
        return None;
    }
    let seps2 = leading_seps(&after[5..]);
    if seps2 == 0 {
        return None;
    }
    let sep = after[5..].chars().nth(seps2 - 1);
    let n = name_len(&after[5 + seps2..], sep)?;
    Some(2 + seps + 5 + seps2 + n)
}

/// `/home/<name>` and `/Users/<name>`.
fn unix_home(s: &str) -> Option<usize> {
    ["/home/", "/Users/"].iter().find_map(|root| {
        let rest = s.strip_prefix(root)?;
        name_len(rest, None).map(|n| root.len() + n)
    })
}

/// `/c/Users/<name>` (Git Bash) and `/mnt/c/Users/<name>` (WSL).
fn msys_home(s: &str) -> Option<usize> {
    let skip = if s.starts_with("/mnt/") { 4 } else { 0 };
    let b = &s.as_bytes()[skip..];
    if b.len() < 3 || b[0] != b'/' || !b[1].is_ascii_alphabetic() || b[2] != b'/' {
        return None;
    }
    let rest = &s[skip + 3..];
    if !rest.get(..5)?.eq_ignore_ascii_case("users") || rest.as_bytes().get(5) != Some(&b'/') {
        return None;
    }
    let n = name_len(&rest[6..], None)?;
    Some(skip + 3 + 6 + n)
}

/// The home directories the caller knows (the current user's), in every spelling a text may use
/// them: as given, both slash styles and, for a drive path, the Git Bash `/c/...` form. Longest first
/// so a home that holds another one wins. The flag says the match ignores ASCII case (drive paths).
fn home_variants(homes: &[&str]) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for home in homes {
        let home = home.trim().trim_end_matches(is_sep);
        if home.len() < 3 {
            continue; // "", "/", "C:" would rewrite far too much
        }
        let b = home.as_bytes();
        let drive = b[1] == b':' && b[0].is_ascii_alphabetic();
        let fwd = home.replace('\\', "/");
        let mut forms = vec![home.to_string(), fwd.clone(), home.replace('/', "\\")];
        if drive {
            forms.push(format!("/{}{}", home[..1].to_ascii_lowercase(), &fwd[2..]));
        }
        for f in forms {
            if !out.iter().any(|(x, _)| *x == f) {
                out.push((f, drive));
            }
        }
    }
    out.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
    out
}

fn known_home(s: &str, variants: &[(String, bool)]) -> Option<usize> {
    let b = s.as_bytes();
    variants.iter().find_map(|(home, ignore_case)| {
        let head = b.get(..home.len())?;
        let same = if *ignore_case {
            head.eq_ignore_ascii_case(home.as_bytes())
        } else {
            head == home.as_bytes()
        };
        (same && !continues_name(b, home.len())).then_some(home.len())
    })
}

/// A path starts after whitespace, a quote, `=`, `:` and the like, not in the middle of a word or of
/// another path. `file:///home/x` counts as a start.
fn path_starts_here(text: &str, i: usize) -> bool {
    let before = &text[..i];
    match before.chars().next_back() {
        None => true,
        Some(c) if c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '~') => false,
        Some('/') => before.ends_with("file://") || before.ends_with("file:///"),
        Some(_) => true,
    }
}

/// Rewrites every path under a home directory in `text` to `~` plus the rest of the path.
///
/// `homes` are the home directories the caller knows (the current user's, from the environment);
/// besides them, any `X:\Users\<name>`, `X:/Users/<name>`, `/home/<name>`, `/Users/<name>`,
/// `/c/Users/<name>` and `/mnt/c/Users/<name>` is rewritten, 8.3 short names (`RUNNER~1`) included.
/// A path that is not under a home directory, and text without paths, comes back borrowed and
/// unchanged. The function reads no environment: an empty or one-character-root `homes` entry is
/// ignored.
pub fn redact_home_paths<'a>(text: &'a str, homes: &[&str]) -> Cow<'a, str> {
    let variants = home_variants(homes);
    let mut out = String::new();
    let (mut last, mut i) = (0, 0);
    while i < text.len() {
        let starts = text.is_char_boundary(i)
            && (text.as_bytes()[i].is_ascii_alphabetic() || text.as_bytes()[i] == b'/')
            && path_starts_here(text, i);
        let hit = if starts {
            let s = &text[i..];
            known_home(s, &variants)
                .or_else(|| windows_home(s))
                .or_else(|| msys_home(s))
                .or_else(|| unix_home(s))
        } else {
            None
        };
        if let Some(len) = hit {
            out.push_str(&text[last..i]);
            out.push('~');
            i += len;
            last = i;
        } else {
            i += 1;
        }
    }
    if last == 0 {
        return Cow::Borrowed(text);
    }
    out.push_str(&text[last..]);
    Cow::Owned(out)
}

// ---- e-mail addresses and secret-like strings ----

/// What a scanned text holds. The match itself is never kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingKind {
    Email,
    /// A well-known key prefix (`sk-`, `ghp_`, `AKIA`, ...) followed by a long token.
    ApiKey,
    /// A PEM private-key header.
    PrivateKey,
    /// `token=`, `password=`, `api_key=` and similar followed by a long value.
    SecretAssignment,
}

impl FindingKind {
    /// Stable slug for JSON and human output.
    pub fn label(self) -> &'static str {
        match self {
            FindingKind::Email => "email",
            FindingKind::ApiKey => "api-key",
            FindingKind::PrivateKey => "private-key",
            FindingKind::SecretAssignment => "secret-assignment",
        }
    }
}

/// Shortest value that counts as "long" after `token=` and its relatives.
const MIN_SECRET_VALUE: usize = 16;

/// (prefix, shortest tail) of well-known credentials. The tail is `[A-Za-z0-9_-]`.
const KEY_PREFIXES: &[(&str, usize)] = &[
    ("sk-", 20),
    ("ghp_", 30),
    ("gho_", 30),
    ("ghu_", 30),
    ("ghs_", 30),
    ("ghr_", 30),
    ("github_pat_", 20),
    ("glpat-", 20),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxa-", 10),
    ("xoxs-", 10),
    ("AKIA", 16),
    ("ASIA", 16),
    ("AIza", 35),
    ("npm_", 30),
    ("hf_", 30),
    ("pypi-", 30),
    ("sk_live_", 16),
    ("rk_live_", 16),
];

const SECRET_KEYS: &[&str] = &[
    "token", "password", "passwd", "secret", "api_key", "api-key", "apikey",
];

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-')
}

/// Addresses the export may hold without a warning: GitHub's private no-reply form and the
/// reserved example domains (the fixtures of this repository use `t@example.com`).
fn is_exempt_domain(domain: &str) -> bool {
    let d = domain.to_ascii_lowercase();
    d == "users.noreply.github.com"
        || d == "example.com"
        || d == "example.org"
        || d.ends_with(".example.com")
        || d.ends_with(".example.org")
}

fn has_email(text: &str) -> bool {
    text.match_indices('@').any(|(at, _)| {
        let local = text[..at]
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_alphanumeric() || "._%+-".contains(*c))
            .count();
        if local == 0 {
            return false;
        }
        let rest = &text[at + 1..];
        let dlen = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
            .unwrap_or(rest.len());
        let domain = rest[..dlen].trim_end_matches('.');
        // `git@github.com:owner/repo` is a remote URL, not a mailbox
        let scp = rest[dlen..]
            .strip_prefix(':')
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '/' || c == '~');
        let tld = domain.rsplit('.').next().unwrap_or("");
        !scp && domain.contains('.')
            && domain.split('.').all(|l| !l.is_empty())
            && tld.len() >= 2
            && tld.chars().all(|c| c.is_ascii_alphabetic())
            && !is_exempt_domain(domain)
    })
}

fn has_api_key(text: &str) -> bool {
    KEY_PREFIXES.iter().any(|(prefix, min)| {
        text.match_indices(prefix).any(|(i, _)| {
            let starts = text[..i]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '_' | '-')));
            starts
                && text[i + prefix.len()..]
                    .chars()
                    .take_while(|c| is_key_char(*c))
                    .count()
                    >= *min
        })
    })
}

fn has_private_key(text: &str) -> bool {
    text.match_indices("-----BEGIN ").any(|(i, _)| {
        text[i..]
            .lines()
            .next()
            .is_some_and(|line| line.contains("PRIVATE KEY-----"))
    })
}

fn has_secret_assignment(text: &str) -> bool {
    let lower = text.to_ascii_lowercase(); // same byte offsets as `text`
    SECRET_KEYS.iter().any(|key| {
        lower.match_indices(key).any(|(i, _)| {
            let mut rest = &text[i + key.len()..];
            let quoted = rest.starts_with(['"', '\'']);
            if quoted {
                rest = &rest[1..];
            }
            rest = rest.trim_start_matches([' ', '\t']);
            // `key=value`; `"key": "value"` only when the key itself was quoted
            let Some(rest) = rest
                .strip_prefix('=')
                .or_else(|| rest.strip_prefix(':').filter(|_| quoted))
            else {
                return false;
            };
            let rest = rest.trim_start_matches([' ', '\t']);
            let rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest);
            rest.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '/' | '=' | '.'))
                .count()
                >= MIN_SECRET_VALUE
        })
    })
}

/// The kinds of e-mail address and secret-like string `text` holds: each kind at most once, in a
/// fixed order, so the same text always gives the same list. The values are not returned.
pub fn scan_findings(text: &str) -> Vec<FindingKind> {
    [
        (FindingKind::Email, has_email(text)),
        (FindingKind::ApiKey, has_api_key(text)),
        (FindingKind::PrivateKey, has_private_key(text)),
        (FindingKind::SecretAssignment, has_secret_assignment(text)),
    ]
    .into_iter()
    .filter_map(|(kind, found)| found.then_some(kind))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(text: &str) -> String {
        redact_home_paths(text, &[]).into_owned()
    }

    #[test]
    fn windows_backslash_and_slash_any_drive() {
        assert_eq!(redact(r"open C:\Users\anna\src\a.rs now"), r"open ~\src\a.rs now");
        assert_eq!(redact("open C:/Users/anna/src/a.rs now"), "open ~/src/a.rs now");
        assert_eq!(redact(r"D:\Users\anna\x"), r"~\x");
        assert_eq!(redact(r"c:\users\anna\x"), r"~\x");
        assert_eq!(redact(r"\\?\C:\Users\anna\x"), r"\\?\~\x");
        assert_eq!(
            redact(r"C:\\Users\\anna\\x"),
            r"~\\x",
            "doubled backslashes of a quoted command"
        );
    }

    #[test]
    fn unix_and_mac_homes() {
        assert_eq!(redact("cat /home/anna/.ssh/config"), "cat ~/.ssh/config");
        assert_eq!(redact("cat /Users/anna/Library/x"), "cat ~/Library/x");
        assert_eq!(redact("PATH=/usr/bin:/home/anna/bin"), "PATH=/usr/bin:~/bin");
        assert_eq!(redact("\"/home/anna/x\" and '/home/anna/y'"), "\"~/x\" and '~/y'");
        assert_eq!(redact("see file:///home/anna/x"), "see file://~/x");
    }

    #[test]
    fn git_bash_and_wsl_forms() {
        assert_eq!(redact("cd /c/Users/anna/proj"), "cd ~/proj");
        assert_eq!(redact("cd /mnt/d/Users/anna/proj"), "cd ~/proj");
    }

    #[test]
    fn the_8_3_short_form_counts() {
        assert_eq!(
            redact(r"C:\Users\RUNNER~1\AppData\Local\Temp\x"),
            r"~\AppData\Local\Temp\x"
        );
        assert_eq!(redact("/home/RUNNER~1/x"), "~/x");
    }

    #[test]
    fn a_name_with_spaces_goes_whole_when_a_separator_closes_it() {
        assert_eq!(redact(r"C:\Users\Max Muster\Desktop\a.txt"), r"~\Desktop\a.txt");
        assert_eq!(redact("C:/Users/Zoë Müller/a"), "~/a");
        // not closed by a separator: only the first word is the name, the prose stays
        assert_eq!(redact(r"the dir C:\Users\anna is big"), "the dir ~ is big");
        assert_eq!(redact(r"C:\Users\anna and D:\work"), r"~ and D:\work");
    }

    /// Prose after a home path must survive, also when a separator follows it further on.
    #[test]
    fn a_spaced_name_never_swallows_prose_up_to_the_next_separator() {
        assert_eq!(
            redact("files in C:\\Users\\anna and src/lib.rs"),
            "files in ~ and src/lib.rs"
        );
        assert_eq!(redact(r"C:\Users\anna and src\lib.rs"), r"~ and src\lib.rs");
        assert_eq!(redact(r"C:\Users\anna in the repo\x"), r"~ in the repo\x");
        // the closing separator must be the one that follows `Users`, and later words are capitalised
        assert_eq!(redact(r"C:\Users\Eva Maria Muster\Desktop"), r"~\Desktop");
        assert_eq!(redact("C:/Users/Max Muster/x"), "~/x");
        assert_eq!(
            redact(r"C:\Users\Max Muster/x"),
            r"~ Muster/x",
            "mixed separators: only the first word"
        );
    }

    #[test]
    fn a_trailing_period_is_not_part_of_the_name() {
        assert_eq!(redact("it lives in /home/anna."), "it lives in ~.");
        assert_eq!(redact("in /home/john.doe/x, ok"), "in ~/x, ok");
    }

    #[test]
    fn the_current_users_home_is_rewritten_in_every_spelling() {
        let homes = [r"D:\Dev\me\"];
        let r = |t: &str| redact_home_paths(t, &homes).into_owned();
        assert_eq!(r(r"D:\Dev\me\proj\a.rs"), r"~\proj\a.rs");
        assert_eq!(r("D:/Dev/me/proj/a.rs"), "~/proj/a.rs");
        assert_eq!(r("d:\\dev\\ME\\proj"), "~\\proj");
        assert_eq!(r("/d/Dev/me/proj"), "~/proj");
        assert_eq!(r("home is D:\\Dev\\me."), "home is ~.");
        // another account whose name starts the same is not the current user's home
        assert_eq!(r(r"D:\Dev\mel\x"), r"D:\Dev\mel\x");
        assert_eq!(r(r"D:\Dev\me2\x"), r"D:\Dev\me2\x");
        let posix = redact_home_paths("cd /srv/people/kim/x", &["/srv/people/kim"]);
        assert_eq!(posix, "cd ~/x");
    }

    #[test]
    fn a_root_or_empty_home_is_ignored() {
        for home in ["", "/", "C:", r"C:\", " "] {
            let t = r"keep /usr/lib and C:\Windows\x";
            assert_eq!(redact_home_paths(t, &[home]), t, "{home:?}");
        }
    }

    #[test]
    fn paths_outside_a_home_directory_are_left_alone() {
        for t in [
            r"C:\Windows\System32\cmd.exe",
            r"D:\Projects\widgets\x",
            "/usr/local/bin/klobuk",
            "/home/",
            "/home",
            "/Users/",
            r"C:\Users",
            r"C:\Users\",
            "src/home/anna/x",
            "/var/home/anna/x",
            r"xC:\Users\anna\x",
            "~/already/short",
            "see /homework/anna",
        ] {
            assert_eq!(redact(t), t, "{t}");
        }
    }

    #[test]
    fn text_without_paths_is_borrowed_and_unchanged() {
        for t in ["", "plain text, nothing here", "ünïcode — ok ✓", "a/b and c\\d"] {
            let out = redact_home_paths(t, &["/home/anna"]);
            assert!(matches!(out, Cow::Borrowed(_)), "{t}");
            assert_eq!(out, t);
        }
    }

    #[test]
    fn a_path_that_appears_several_times_is_rewritten_every_time() {
        let t = r"C:\Users\anna\a and C:\Users\anna\b, again C:/Users/anna/c; /home/anna/d /home/ben/e";
        assert_eq!(redact(t), r"~\a and ~\b, again ~/c; ~/d ~/e");
        let twice = redact_home_paths("/srv/u/x /srv/u/y", &["/srv/u"]);
        assert_eq!(twice, "~/x ~/y");
    }

    #[test]
    fn the_result_is_stable_when_applied_again() {
        let once = redact(r"C:\Users\anna\a /home/ben/b");
        assert_eq!(redact(&once), once);
    }

    // ---- findings ----

    fn kinds(t: &str) -> Vec<&'static str> {
        scan_findings(t).into_iter().map(FindingKind::label).collect()
    }

    #[test]
    fn real_addresses_are_found() {
        assert_eq!(kinds("write to anna.k@corp.io please"), ["email"]);
        assert_eq!(kinds("<dev+tag@mail.co.uk>"), ["email"]);
        assert_eq!(kinds("a@b.com: thanks"), ["email"]);
    }

    #[test]
    fn noreply_example_and_fixture_addresses_do_not_warn() {
        for t in [
            "12345+anna@users.noreply.github.com",
            "t@example.com",
            "x@sub.example.com",
            "t@example.org",
            "t@x",
            "a@b",
            "git@github.com:acme/widgets.git",
            "serde@1.0.200",
            "install @types/node@20.1",
            "no address here",
        ] {
            assert!(kinds(t).is_empty(), "{t}: {:?}", kinds(t));
        }
    }

    #[test]
    fn well_known_key_prefixes_are_found() {
        for t in [
            "key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
            "xoxb-123456789012-abcdef",
        ] {
            assert_eq!(kinds(t), ["api-key"], "{t}");
        }
        for t in [
            "sk-short",
            "disk-usage-report-for-all-nodes-today",
            "ghp_short",
            "task-queue-management-system",
        ] {
            assert!(kinds(t).is_empty(), "{t}");
        }
    }

    #[test]
    fn a_pem_private_key_header_is_found() {
        assert_eq!(kinds("-----BEGIN RSA PRIVATE KEY-----\nMIIE"), ["private-key"]);
        assert_eq!(kinds("-----BEGIN OPENSSH PRIVATE KEY-----"), ["private-key"]);
        assert!(kinds("-----BEGIN CERTIFICATE-----").is_empty());
        assert!(kinds("-----BEGIN PUBLIC KEY-----").is_empty());
    }

    #[test]
    fn long_values_after_secret_names_are_found() {
        for t in [
            "token=abcdef0123456789abcdef",
            "PASSWORD = hunter2hunter2hunter2",
            "export GITHUB_TOKEN=Zm9vYmFyYmF6cXV4MTIzNDU2",
            "api_key='abcdef0123456789abcd'",
            r#"{"token": "abcdef0123456789abcdef"}"#,
            "https://x.test/?apikey=abcdef0123456789abcd&b=1",
        ] {
            assert_eq!(kinds(t), ["secret-assignment"], "{t}");
        }
        for t in [
            "token=short",
            "password=",
            "max_tokens=100000000000000000",
            "token: refreshed_successfully_now_ok",
            "password=${PASSWORD_FROM_ENV_VARIABLE}",
            "the token is rotated daily",
        ] {
            assert!(kinds(t).is_empty(), "{t}: {:?}", kinds(t));
        }
    }

    #[test]
    fn each_kind_is_reported_once_in_a_fixed_order() {
        let t = "a@corp.io b@corp.io token=abcdef0123456789abcdef sk-abcdefghijklmnopqrstuvwxyz";
        assert_eq!(kinds(t), ["email", "api-key", "secret-assignment"]);
        assert_eq!(scan_findings(t), scan_findings(t));
    }

    #[test]
    fn a_finding_never_holds_the_value() {
        // FindingKind has no payload, so there is nothing to print: pin the label set
        let all = [
            FindingKind::Email,
            FindingKind::ApiKey,
            FindingKind::PrivateKey,
            FindingKind::SecretAssignment,
        ];
        let labels: Vec<_> = all.iter().map(|k| k.label()).collect();
        assert_eq!(labels, ["email", "api-key", "private-key", "secret-assignment"]);
    }
}
