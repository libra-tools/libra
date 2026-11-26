//! Pure HTTP redirect policy builder sunk from `internal::protocol::https_client`.

/// Build a redirect policy that refuses HTTPS-to-HTTP downgrade redirects and
/// caps redirect chains at 10 hops.
#[doc(hidden)]
pub fn no_downgrade_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let downgraded = attempt.url().scheme() == "http"
            && attempt
                .previous()
                .last()
                .is_some_and(|previous| previous.scheme() == "https");
        if downgraded {
            attempt.error("refusing an https->http downgrade redirect")
        } else if attempt.previous().len() > 10 {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}
