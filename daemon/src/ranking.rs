//! The single scorer behind every result (contract §5). The extension mirrors this in
//! `extension/lib/scoring.js`; the two must stay in step.

use crate::protocol::{ResultItem, PROVIDER_IDS};

/// The provider's base weight, from contract §5.
pub fn base_weight(provider: &str) -> f64 {
    match provider {
        "applications" => 0.30,
        "calculator" => 0.40,
        "actions" => 0.25,
        "settings" => 0.25,
        "files" => 0.20,
        "recent" => 0.18,
        "clipboard" => 0.15,
        "websearch" => 0.05,
        _ => 0.0,
    }
}

/// How well `title` matches `query`: exact > prefix > word-start > substring > fuzzy.
pub fn match_quality(title: &str, query: &str) -> f64 {
    let text = title.to_lowercase();
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return 0.0;
    }

    if text == needle {
        return 1.0;
    }
    if text.starts_with(&needle) {
        return 0.8;
    }
    if text
        .split(|c: char| c.is_whitespace() || matches!(c, '-' | '_' | '/' | '.'))
        .any(|word| word.starts_with(&needle))
    {
        return 0.6;
    }
    if text.contains(&needle) {
        return 0.4;
    }
    if is_subsequence(&text, &needle) {
        return 0.2;
    }
    0.0
}

fn is_subsequence(text: &str, needle: &str) -> bool {
    let mut chars = text.chars();
    needle
        .chars()
        .all(|wanted| chars.any(|candidate| candidate == wanted))
}

/// Recency decays linearly to zero over 30 days.
pub fn recency_bonus(timestamp_ms: Option<i64>, now_ms: i64) -> f64 {
    let Some(timestamp) = timestamp_ms else {
        return 0.0;
    };
    const HORIZON_MS: f64 = 30.0 * 24.0 * 60.0 * 60.0 * 1000.0;
    let age = (now_ms - timestamp).max(0) as f64;
    (0.10 * (1.0 - age / HORIZON_MS)).clamp(0.0, 0.10)
}

/// Usage rises linearly to the cap at 20 activations.
pub fn usage_bonus(usage: u32) -> f64 {
    const CAP: f64 = 20.0;
    0.20 * (usage as f64).min(CAP) / CAP
}

/// The final score: base weight + match quality + recency + usage.
///
/// The web-search row is the exception: its title is spelled out of the query ("Search the web for
/// …"), so matching it against the query would always score it as a hit and float the last-resort
/// row above real results. It is scored by its base weight alone (ticket 08, contract §15).
pub fn score(item: &ResultItem, query: &str, now_ms: i64) -> f64 {
    let quality = if item.provider == "websearch" {
        0.0
    } else {
        match_quality(&item.title, query)
    };

    base_weight(&item.provider) + quality + recency_bonus(item.timestamp, now_ms) + usage_bonus(item.usage)
}

/// The tie-break position of a provider in the section order.
pub fn provider_rank(provider: &str) -> usize {
    PROVIDER_IDS
        .iter()
        .position(|id| *id == provider)
        .unwrap_or(PROVIDER_IDS.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(provider: &str, title: &str) -> ResultItem {
        ResultItem::new(provider, format!("{provider}:x"), title)
    }

    #[test]
    fn match_quality_ranks_exact_over_prefix_over_word_start_over_substring_over_fuzzy() {
        assert_eq!(match_quality("Terminal", "terminal"), 1.0);
        assert_eq!(match_quality("Terminal Emulator", "term"), 0.8);
        assert_eq!(match_quality("GNOME Terminal", "term"), 0.6);
        assert_eq!(match_quality("GNOME Terminal", "minal"), 0.4);
        assert_eq!(match_quality("Terminal", "trmnl"), 0.2);
        assert_eq!(match_quality("Terminal", "zzz"), 0.0);
        assert_eq!(match_quality("Terminal", ""), 0.0);
    }

    #[test]
    fn an_exact_application_match_outranks_an_exact_file_match() {
        let now = 0;
        let app = score(&item("applications", "Files"), "files", now);
        let file = score(&item("files", "Files"), "files", now);
        assert!(app > file, "{app} must beat {file}");
        assert!((app - 1.30).abs() < 1e-9);
    }

    #[test]
    fn recency_and_usage_are_bounded() {
        assert_eq!(recency_bonus(None, 0), 0.0);
        assert_eq!(recency_bonus(Some(0), 0), 0.10);
        assert!(recency_bonus(Some(-1000), 0) <= 0.10);
        assert_eq!(usage_bonus(0), 0.0);
        assert!((usage_bonus(20) - 0.20).abs() < 1e-9);
        assert!((usage_bonus(1000) - 0.20).abs() < 1e-9);
    }

    #[test]
    fn provider_rank_follows_the_section_order() {
        assert!(provider_rank("applications") < provider_rank("files"));
        assert!(provider_rank("files") < provider_rank("websearch"));
    }

    #[test]
    fn the_web_search_row_is_scored_by_its_base_weight_alone() {
        let row = item("websearch", "Search the web for term");
        assert!(
            (score(&row, "term", 0) - 0.05).abs() < 1e-9,
            "the row's title is built from the query, so it cannot be matched against it"
        );
        assert!(
            score(&item("files", "xterm-settings.txt"), "term", 0) > score(&row, "term", 0),
            "and a real result always outranks the last resort"
        );
    }
}
