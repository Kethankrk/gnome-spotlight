//! Which clipboard text looks like a secret.
//!
//! Users copy passwords, API tokens and private keys by accident, and history is exactly the wrong
//! place for them: it is on disk, it is searchable, and it outlives the session. The rule here is
//! therefore to err towards suppression — a suppressed copy costs the user one re-copy, a leaked
//! password costs them the account.
//!
//! The heuristic is a handful of rules over the text alone: pure, no allocation-heavy parsing, and
//! no network, so it is cheap on every copy and fully testable (see the table at the bottom). The
//! rules, in the order they are tried:
//!
//! 1. **private-key-block** — a PEM/OpenSSH private key block.
//! 2. **jwt** — a JSON Web Token: three base64url segments, the first a base64 `{"…` header.
//! 3. **assigned-credential** — a credential word (`password`, `token`, `secret`, `api_key`, …)
//!    followed by `:` or `=` and a single value of six characters or more.
//! 4. **bearer-token** — `Bearer` followed by a token of sixteen characters or more.
//! 5. **url-credentials** — `scheme://user:password@host`, the shape of a pasted database URL.
//! 6. **aws-key-id** — an `AKIA…` access key id.
//! 7. **random-token** — the *whole* copy is one long, mixed-class token (32 characters or more,
//!    at least three of lower/upper/digit/symbol). Text with spaces in it, a URL, or a path is not
//!    a token, so a copied sentence or file location is never suppressed by this rule.
//!
//! `Copy` of ordinary prose, URLs, paths and git hashes is not suppressed; the table below is the
//! specification.

/// The rules, by name, for tests and diagnostics.
pub const RULES: [&str; 7] = [
    "private-key-block",
    "jwt",
    "assigned-credential",
    "bearer-token",
    "url-credentials",
    "aws-key-id",
    "random-token",
];

/// The shortest copy the `random-token` rule will call a token.
const RANDOM_TOKEN_MIN: usize = 32;
/// The shortest credential value worth calling a credential.
const CREDENTIAL_VALUE_MIN: usize = 6;
/// The shortest bearer token worth calling a token.
const BEARER_MIN: usize = 16;

/// The words that, followed by `:` or `=` and a value, mark a credential assignment.
const CREDENTIAL_WORDS: [&str; 15] = [
    "password",
    "passwd",
    "passphrase",
    "pwd",
    "secret",
    "client_secret",
    "client secret",
    "token",
    "api_key",
    "api key",
    "apikey",
    "access_key",
    "access key",
    "private_key",
    "credential",
];

/// Does this copied text look like a secret?
pub fn is_secret(text: &str) -> bool {
    matched_rule(text).is_some()
}

/// The first rule that matched, or `None` for text that is safe to keep. The name is one of
/// [`RULES`].
pub fn matched_rule(text: &str) -> Option<&'static str> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();

    if is_private_key_block(&lower) {
        return Some("private-key-block");
    }
    if is_jwt(text) {
        return Some("jwt");
    }
    if has_assigned_credential(&lower) {
        return Some("assigned-credential");
    }
    if has_bearer_token(&lower) {
        return Some("bearer-token");
    }
    if has_url_credentials(text) {
        return Some("url-credentials");
    }
    if has_aws_key_id(text) {
        return Some("aws-key-id");
    }
    if is_random_token(text) {
        return Some("random-token");
    }
    None
}

fn is_private_key_block(lower: &str) -> bool {
    lower.contains("-----begin") && lower.contains("private key")
}

fn is_jwt(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    let [header, payload, signature] = parts[..] else {
        return false;
    };

    header.starts_with("eyJ") // base64url of `{"`
        && text.len() >= 40
        && !payload.is_empty()
        && [header, payload, signature]
            .iter()
            .all(|part| part.bytes().all(is_base64url_byte))
}

fn is_base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'='
}

fn has_assigned_credential(lower: &str) -> bool {
    CREDENTIAL_WORDS.iter().any(|word| {
        let mut from = 0;
        while let Some(index) = lower[from..].find(word) {
            let after_word = from + index + word.len();
            if let Some(value) = credential_value(&lower[after_word..]) {
                return value.chars().count() >= CREDENTIAL_VALUE_MIN;
            }
            from = after_word;
            if from >= lower.len() {
                break;
            }
        }
        false
    })
}

/// The value that follows a credential word: `[:=] *<token>`.
fn credential_value(after_word: &str) -> Option<&str> {
    let rest = after_word.trim_start_matches(|c: char| c.is_whitespace() || c == '"' || c == '\'');
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('='))?;
    let rest = rest.trim_start_matches([' ', '\t', '"', '\'']);

    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == ';')
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

fn has_bearer_token(lower: &str) -> bool {
    let mut from = 0;
    while let Some(index) = lower[from..].find("bearer") {
        let after = from + index + "bearer".len();
        let rest = lower[after..].trim_start_matches(' ');
        let token: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '\'')
            .collect();
        if token.chars().count() >= BEARER_MIN {
            return true;
        }
        from = after;
        if from >= lower.len() {
            break;
        }
    }
    false
}

fn has_url_credentials(text: &str) -> bool {
    let Some((_, after_scheme)) = text.split_once("://") else {
        return false;
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    match authority.rsplit_once('@') {
        // `redis://:password@host` is a credential with no user name, and just as much of one.
        Some((userinfo, _host)) => match userinfo.split_once(':') {
            Some((_user, password)) => !password.is_empty(),
            None => false,
        },
        None => false,
    }
}

fn has_aws_key_id(text: &str) -> bool {
    text.match_indices("AKIA").any(|(index, _)| {
        text[index + 4..]
            .chars()
            .take(16)
            .filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            .count()
            == 16
    })
}

fn is_random_token(text: &str) -> bool {
    if text.len() < RANDOM_TOKEN_MIN
        || text.contains(char::is_whitespace)
        || text.contains("://")
        || text.starts_with('/')
        || text.starts_with('~')
        || text.starts_with('.')
    {
        return false;
    }

    let mut lower = false;
    let mut upper = false;
    let mut digit = false;
    let mut symbol = false;
    for byte in text.bytes() {
        match byte {
            b'a'..=b'z' => lower = true,
            b'A'..=b'Z' => upper = true,
            b'0'..=b'9' => digit = true,
            b'+' | b'/' | b'=' | b'_' | b'-' | b'.' | b':' | b'@' => symbol = true,
            _ => return false, // anything else is not a token
        }
    }
    [lower, upper, digit, symbol]
        .iter()
        .filter(|present| **present)
        .count()
        >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The specification: (text, is it a secret, the rule that must match).
    const TABLE: &[(&str, bool, Option<&str>)] = &[
        // ---- ordinary clipboard content: kept ----
        ("the quick brown fox", false, None),
        ("https://example.com/pricing", false, None),
        ("https://github.com/kethan/lumen/pull/11", false, None),
        ("/home/kethan/Projects/Lumen/README.md", false, None),
        ("~/Projects/Lumen-2024/notes.md", false, None),
        ("a secret nobody should find on disk", false, None),
        ("password", false, None),
        ("my password is not here", false, None),
        ("token", false, None),
        ("Meeting at 10:30 about the api_key rotation", false, None),
        ("4b825dc642cb6eb9a060e54bf8d69288fbee4904", false, None),
        ("fn main() { println!(\"hi\"); }", false, None),
        ("SELECT * FROM notes WHERE id = 42;", false, None),
        ("", false, None),
        ("   ", false, None),
        ("password:", false, None),
        ("password: ", false, None),
        ("token = abc", false, None), // three characters is not a value worth hiding
        // ---- secrets: suppressed ----
        (
            "password=hunter2",
            true,
            Some("assigned-credential"),
        ),
        ("passwd: correct-horse", true, Some("assigned-credential")),
        ("PWD = s3cret!", true, Some("assigned-credential")),
        (
            "{\"password\": \"hunter2\"}",
            true,
            Some("assigned-credential"),
        ),
        (
            "api_key=sk_live_9f8e7d6c5b4a",
            true,
            Some("assigned-credential"),
        ),
        (
            "client_secret: 1a2b3c4d5e6f7g8h",
            true,
            Some("assigned-credential"),
        ),
        (
            "Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345",
            true,
            Some("bearer-token"),
        ),
        (
            "postgres://lumen:hunter2@db.example.com:5432/lumen",
            true,
            Some("url-credentials"),
        ),
        (
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----",
            true,
            Some("private-key-block"),
        ),
        (
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----",
            true,
            Some("private-key-block"),
        ),
        (
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U",
            true,
            Some("jwt"),
        ),
        ("AKIAIOSFODNN7EXAMPLE", true, Some("aws-key-id")),
        ("Zx9Qw2Er7Ty4Ui6Op1As3Df5Gh8Jk0Lz", true, Some("random-token")),
        (
            "ghp_9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b2a",
            true,
            Some("random-token"),
        ),
        (
            "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQ",
            true,
            Some("random-token"),
        ),
    ];

    #[test]
    fn the_table_holds() {
        for (text, expected, rule) in TABLE {
            assert_eq!(
                is_secret(text),
                *expected,
                "`{text}` should {}be a secret",
                if *expected { "" } else { "not " }
            );
            assert_eq!(
                matched_rule(text),
                *rule,
                "`{text}` matched the wrong rule"
            );
        }
    }

    #[test]
    fn every_reported_rule_is_one_of_the_named_ones() {
        for (text, expected, _) in TABLE {
            if *expected {
                let rule = matched_rule(text).expect("a secret has a rule");
                assert!(RULES.contains(&rule), "`{rule}` is not a named rule");
            }
        }
    }

    #[test]
    fn suppression_is_whitespace_insensitive_at_the_edges() {
        assert!(is_secret("  password=hunter2\n"));
        assert!(!is_secret("  hello world  "));
    }

    #[test]
    fn a_credential_word_in_the_middle_of_prose_is_not_an_assignment() {
        assert!(!has_assigned_credential("the password reset link expires"));
        assert!(has_assigned_credential("password: letmein"));
        assert!(has_assigned_credential("secret=0123456789"));
    }

    #[test]
    fn a_url_without_a_password_keeps_its_place_in_history() {
        assert!(!has_url_credentials("https://user@example.com/x"));
        assert!(!has_url_credentials("https://example.com/a:b@c"));
        assert!(has_url_credentials("redis://:hunter2@cache:6379"));
    }

    #[test]
    fn a_long_word_is_not_a_token() {
        assert!(!is_random_token(&"a".repeat(64)), "one class is not a token");
        assert!(!is_random_token(&format!("{} notes", "Zx9Qw2Er7Ty4Ui6Op1As3Df5Gh8Jk0Lz")));
        assert!(is_random_token(&"Zx9Qw2Er7Ty4Ui6Op1As3Df5Gh8Jk0Lz"));
    }
}
