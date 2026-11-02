//! Local search admission. Invalid queries never become HTTP requests.

use crate::internal::protocol::mega2_traces::validate_search_query;

pub fn admit_search_query(query: &str) -> Result<(), String> {
    validate_search_query(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_argument_bounds() {
        assert!(admit_search_query("").is_err(), "0 bytes are rejected");
        assert!(
            admit_search_query(&"a".repeat(257)).is_err(),
            "257 bytes are rejected"
        );
        assert!(admit_search_query("a").is_ok(), "1 byte is allowed");
        assert!(
            admit_search_query(&"a".repeat(256)).is_ok(),
            "256 bytes are allowed"
        );
        assert!(
            admit_search_query("你").is_ok(),
            "a multibyte character counts as its UTF-8 bytes"
        );
        assert!(
            admit_search_query(&"你".repeat(86)).is_err(),
            "86 CJK characters are 258 bytes"
        );
    }
}
