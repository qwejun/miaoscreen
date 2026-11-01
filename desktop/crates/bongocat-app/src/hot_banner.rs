//! Trending topics, as one line of text for the panel's banner.
//!
//! The panel draws whatever string arrives and knows nothing about where it came
//! from, so this is a host-side concern from end to end: fetch a list, pick one
//! title, hand it to the strip. Nothing here is persisted and nothing here is
//! part of the wire protocol.
//!
//! Two mirrors of the same list are tried in order. Both are public JSON
//! endpoints that answer with `{"data": [{"title": …}, …]}`, which is the only
//! shape this module depends on — a source that renames its fields shows up as
//! "no titles", never as a wrong line on the panel.

use std::time::Duration;

use cargo_packager_updater::reqwest;

/// The mirrors, most preferred first.
///
/// The first is the open-source `60s` API, the second a separate aggregator;
/// they are different services rather than two paths to one, which is the point:
/// the usual failure here is one host rate-limiting this machine, and a second
/// host is what makes that a hiccup instead of an outage.
const SOURCES: [&str; 2] = [
    "https://60s.viki.moe/v2/weibo",
    "https://v2.xxapi.cn/api/weibohot",
];

/// One request may not hold the worker longer than this.
///
/// The worker refreshes every three minutes, so a source that accepts the
/// connection and never answers must not be able to eat the next interval.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// The longest list this keeps. Only the top of a trending list is worth
/// showing, and a title past this rank is rarely a sentence.
const MAX_TITLES: usize = 10;

/// The longest title the banner will take, in characters.
///
/// The panel wraps, but a trending title that is a paragraph reads as a smear at
/// 30px on a round screen; those are dropped rather than truncated, because half
/// a sentence is worse than the next item.
const MAX_TITLE_CHARS: usize = 40;

/// Fetches the current list and returns up to [`MAX_TITLES`] titles.
///
/// An empty result means "nothing usable right now" — the caller keeps whatever
/// is already on screen, so a network failure costs freshness, not the line.
pub(crate) fn fetch_titles() -> Vec<String> {
    let client = match reqwest::blocking::Client::builder()
        .https_only(true)
        .timeout(REQUEST_TIMEOUT)
        .build()
    {
        Ok(client) => client,
        Err(_) => return Vec::new(),
    };
    for source in SOURCES {
        let Ok(response) = client.get(source).send() else {
            continue;
        };
        let Ok(response) = response.error_for_status() else {
            continue;
        };
        let Ok(body) = response.text() else {
            continue;
        };
        let titles = parse_titles(&body);
        if !titles.is_empty() {
            return titles;
        }
    }
    Vec::new()
}

/// Pulls `data[].title` out of a response body.
///
/// Kept separate from the transport so the parsing rules — which are the only
/// rules that can put a wrong line on the panel — are testable without a network.
fn parse_titles(body: &str) -> Vec<String> {
    let Ok(document) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(rows) = document.get("data").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| row.get("title").and_then(serde_json::Value::as_str))
        .map(str::trim)
        // A line break would take the panel's row count away from the setting
        // that governs it, so a title carrying one is not a title any more.
        .filter(|title| !title.is_empty() && !title.contains(['\n', '\r', '\t']))
        .filter(|title| title.chars().count() <= MAX_TITLE_CHARS)
        .take(MAX_TITLES)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_out_of_the_shared_data_shape() {
        let body = r#"{"code":200,"data":[{"title":"甲","hot_value":1},{"title":" 乙 ","index":2}]}"#;
        assert_eq!(parse_titles(body), vec!["甲".to_owned(), "乙".to_owned()]);
    }

    #[test]
    fn a_body_that_is_not_a_list_yields_nothing_rather_than_a_wrong_line() {
        for body in [
            "",
            "not json",
            "{}",
            r#"{"data":{}}"#,
            r#"{"error":"rate limited"}"#,
            r#"{"data":[{"hot":"1万"}]}"#,
        ] {
            assert!(parse_titles(body).is_empty(), "{body}");
        }
    }

    #[test]
    fn unusable_rows_are_skipped_and_the_list_is_capped() {
        let long = "长".repeat(MAX_TITLE_CHARS + 1);
        let body = format!(
            r#"{{"data":[{{"title":""}},{{"title":"两\n行"}},{{"title":"{long}"}},{{"title":"可用"}}]}}"#
        );
        assert_eq!(parse_titles(&body), vec!["可用".to_owned()]);

        let many: Vec<String> = (0..30).map(|n| format!(r#"{{"title":"第{n}条"}}"#)).collect();
        let body = format!(r#"{{"data":[{}]}}"#, many.join(","));
        assert_eq!(parse_titles(&body).len(), MAX_TITLES);
    }
}
