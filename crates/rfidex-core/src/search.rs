//! Search input rules shared by the mock server and the station.
//!
//! The client applies the same minima the server does, so an input that cannot
//! match never leaves the computer. The server still enforces them: this is a
//! convenience, not the guard.

use crate::contract::SearchBy;

/// Trim, collapse runs of whitespace and lowercase, the way the stored
/// normalised name does.
pub fn normalize_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Trim and lowercase. The `@` check is the caller's, not this function's.
pub fn normalize_email(value: &str) -> String {
    value.trim().to_lowercase()
}

/// Keep ASCII digits, then drop one leading country or trunk prefix, so
/// `012-345 6789`, `0123456789` and `+60 12 345 6789` become one number.
pub fn normalize_phone(value: &str) -> String {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    digits
        .strip_prefix("60")
        .or_else(|| digits.strip_prefix('0'))
        .unwrap_or(&digits)
        .to_owned()
}

/// The query as it will be matched, or `None` when it is too short to match
/// anything. Counting is by character, so a two-character name in any script is
/// long enough.
pub fn normalized_query(by: SearchBy, value: &str) -> Option<String> {
    match by {
        SearchBy::Name => {
            let name = normalize_name(value);
            (name.chars().count() >= 2).then_some(name)
        }
        SearchBy::Email => {
            let email = normalize_email(value);
            email.contains('@').then_some(email)
        }
        SearchBy::Phone => {
            let phone = normalize_phone(value);
            (phone.chars().count() >= 4).then_some(phone)
        }
    }
}

impl SearchBy {
    pub fn as_str(self) -> &'static str {
        match self {
            SearchBy::Name => "name",
            SearchBy::Email => "email",
            SearchBy::Phone => "phone",
        }
    }

    /// `None` for anything the contract does not define, so an unknown field is
    /// a readable rejection rather than a default.
    pub fn parse(value: &str) -> Option<SearchBy> {
        match value {
            "name" => Some(SearchBy::Name),
            "email" => Some(SearchBy::Email),
            "phone" => Some(SearchBy::Phone),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_collapse_case_and_spaces() {
        assert_eq!(normalize_name("  AHMAD   bin "), "ahmad bin");
        assert_eq!(normalize_name("Ahmad\tBin\nAli"), "ahmad bin ali");
        assert_eq!(normalize_name("  "), "");
        assert_eq!(
            normalized_query(SearchBy::Name, "  Ahmad  "),
            Some("ahmad".into())
        );
    }

    #[test]
    fn the_name_minimum_counts_characters_not_bytes() {
        assert_eq!(normalized_query(SearchBy::Name, "a"), None);
        assert_eq!(normalized_query(SearchBy::Name, "  "), None);
        assert_eq!(normalized_query(SearchBy::Name, "安"), None);
        assert!(normalized_query(SearchBy::Name, " 安田 ").is_some());
    }

    #[test]
    fn emails_are_trimmed_lowercased_and_need_an_at_sign() {
        assert_eq!(
            normalized_query(SearchBy::Email, " AHMAD@EXAMPLE.COM "),
            Some("ahmad@example.com".into())
        );
        assert_eq!(normalized_query(SearchBy::Email, "ahmad"), None);
        assert_eq!(normalized_query(SearchBy::Email, "example.com"), None);
        assert_eq!(normalized_query(SearchBy::Email, "  "), None);
    }

    #[test]
    fn phone_prefixes_collapse_and_four_digits_are_the_minimum() {
        for value in ["012-345 6789", "0123456789", "+60 12 345 6789"] {
            assert_eq!(
                normalized_query(SearchBy::Phone, value),
                Some("123456789".into()),
                "{value}"
            );
        }
        assert_eq!(normalized_query(SearchBy::Phone, "012"), None);
        assert_eq!(normalized_query(SearchBy::Phone, "600"), None);
        assert_eq!(normalized_query(SearchBy::Phone, "abc"), None);
        assert_eq!(
            normalized_query(SearchBy::Phone, "6012345"),
            Some("12345".into())
        );
    }

    #[test]
    fn an_unknown_search_field_is_not_guessed() {
        assert_eq!(SearchBy::parse("name"), Some(SearchBy::Name));
        assert_eq!(SearchBy::parse("staff"), None);
        assert_eq!(SearchBy::Name.as_str(), "name");
    }
}
