//! Joining a session from a plain browser, through a share link.
//!
//! A share link is the app's own URL plus `?mirrord-session=<key>`. The browser has no mirrord
//! extension and cannot set headers, so somebody in the cluster has to turn that key into the
//! `baggage: mirrord-session=<key>` header the session's HTTP filter matches on. That is what
//! this module does, for every redirected HTTP request:
//!
//! - The key is taken from the `mirrord-session` query param, or from the `mirrord-session` cookie.
//!   The param wins, so opening a fresh link always switches sessions.
//! - When the key has a live session ([`ShareLinkKeys`]), the request gets the baggage entry and
//!   the param is dropped from the query string, so the app never sees it. A key that arrived in
//!   the param is also pinned in a cookie, so the following requests (page assets, XHRs, links the
//!   viewer clicks) stay in the session without carrying the param.
//! - When the key has no live session, a param gets the [session-ended
//!   page](session_ended_response) and a cookie gets cleared.
//!
//! Keys are compared verbatim: they are plain identifiers, so there is nothing to percent-decode
//! or case-fold.

use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    ops::Not,
    str::FromStr,
    sync::{Arc, LazyLock, PoisonError, RwLock},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{
    HeaderMap, Response, Uri, Version,
    header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, HeaderName, HeaderValue, SET_COOKIE},
    http::{request::Parts, uri::PathAndQuery},
};
use mirrord_protocol::share_link::ShareLinkRequest;
use tokio::sync::oneshot;

use super::BoxResponse;

/// Name of the query param, the cookie and the baggage entry that carry the session key.
const SESSION_KEY: &str = "mirrord-session";

/// Header the session's HTTP filter matches on.
const BAGGAGE: HeaderName = HeaderName::from_static("baggage");

/// Cookie that keeps a browser in the session once the query param is gone.
///
/// It has no `Max-Age`, so it lives only as long as the browser is open: a share link never
/// silently puts somebody back into a session days later.
const JOIN_COOKIE_ATTRIBUTES: &str = "; Path=/; HttpOnly; SameSite=Lax";

/// Cookie value that makes the browser drop a cookie whose session is gone: same cookie as
/// [`JOIN_COOKIE_ATTRIBUTES`], but with an empty value and an immediate expiry.
static CLEAR_COOKIE: LazyLock<HeaderValue> = LazyLock::new(|| {
    HeaderValue::from_str(&format!(
        "{SESSION_KEY}={JOIN_COOKIE_ATTRIBUTES}; Max-Age=0"
    ))
    .expect("the cookie is built from valid static header parts")
});

const HTML_CONTENT_TYPE: HeaderValue = HeaderValue::from_static("text/html; charset=utf-8");

/// The session-ended page must not be cached in place of the app's real response.
const NO_STORE: HeaderValue = HeaderValue::from_static("no-store");

/// Seconds the [session-ended page](session_ended_response) counts down before it continues to
/// the app without the session. Long enough to read the message, short enough not to feel stuck.
const SESSION_ENDED_COUNTDOWN_SECS: u8 = 5;

/// Session keys that share links may join, as registered by the operator.
///
/// The operator owns sessions, so it is the only one that knows which keys are live. One agent
/// outlives the sessions using it, which is why keys are registered and removed over its whole
/// lifetime instead of being fixed at startup.
///
/// Each key counts its registrations, because two client connections may register the same key:
/// it stays active until every one of them lets go.
///
/// Cheap to clone - every clone reads and writes the same set.
#[derive(Clone, Default, Debug)]
pub struct ShareLinkKeys(Arc<RwLock<HashMap<String, usize>>>);

impl ShareLinkKeys {
    /// Makes the view through which one client connection registers its keys.
    pub fn for_client(&self) -> ClientShareLinkKeys {
        ClientShareLinkKeys {
            shared: self.clone(),
            registered: HashSet::new(),
        }
    }

    fn is_active(&self, key: &str) -> bool {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(key)
    }

    fn register(&self, key: &str) {
        *self
            .0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(key.to_owned())
            .or_default() += 1;
    }

    fn release(&self, key: &str) {
        let mut keys = self.0.write().unwrap_or_else(PoisonError::into_inner);
        let Some(registrations) = keys.get_mut(key) else {
            return;
        };

        *registrations = registrations.saturating_sub(1);
        if *registrations == 0 {
            keys.remove(key);
        }
    }

    /// Looks for a session key in the request and rewrites `parts` in place when it finds one.
    ///
    /// Returns what still has to happen to this request's response, or [`None`] when the response
    /// can be passed back untouched.
    pub fn inspect(&self, parts: &mut Parts) -> Option<ShareLinkAction> {
        inspect(parts, |key| self.is_active(key))
    }
}

/// One client connection's handle on [`ShareLinkKeys`].
///
/// Keys live only as long as the connection that registered them: an operator that dies
/// mid-session cannot send [`ShareLinkRequest::RemoveKey`], and without this cleanup a long-lived
/// agent would keep joining viewers to sessions that are gone. After a reconnect the operator
/// registers its keys again, the same way it replays port subscriptions.
#[derive(Debug)]
pub struct ClientShareLinkKeys {
    shared: ShareLinkKeys,
    /// What this connection registered, so dropping the handle releases exactly that - even when
    /// the client sends the same registration or removal twice.
    registered: HashSet<String>,
}

impl ClientShareLinkKeys {
    /// Applies an update from this client.
    pub fn update(&mut self, update: ShareLinkRequest) {
        match update {
            ShareLinkRequest::RegisterKey(key) => {
                if self.registered.insert(key.clone()) {
                    self.shared.register(&key);
                }
            }
            ShareLinkRequest::RemoveKey(key) => {
                if self.registered.remove(&key) {
                    self.shared.release(&key);
                }
            }
        }
    }
}

impl Drop for ClientShareLinkKeys {
    fn drop(&mut self) {
        for key in self.registered.drain() {
            self.shared.release(&key);
        }
    }
}

/// What is left to do about a request that carried a session key, once the request itself has
/// been rewritten.
#[derive(Debug, PartialEq, Eq)]
pub enum ShareLinkAction {
    /// Add this `Set-Cookie` to the response, either pinning the joined session or clearing a
    /// cookie whose session is gone.
    SetCookie(HeaderValue),
    /// Do not forward this request at all. Answer it with the [session-ended
    /// page](session_ended_response), which continues to `continue_to`.
    SessionEnded { continue_to: String },
}

/// Implements [`ShareLinkKeys::inspect`], with the key lookup passed in so it can be tested
/// without a registry.
fn inspect(parts: &mut Parts, is_active: impl Fn(&str) -> bool) -> Option<ShareLinkAction> {
    // The param wins over the cookie, so opening a fresh link always switches sessions.
    let Some(param) = key_param(&parts.uri) else {
        return inspect_cookie(parts, is_active);
    };

    if is_active(&param.value).not() {
        return Some(ShareLinkAction::SessionEnded {
            continue_to: param.without_key,
        });
    }

    match replace_path_and_query(&parts.uri, &param.without_key) {
        Some(uri) => parts.uri = uri,
        // The app now sees the param it would not otherwise get. Better than failing the
        // request, and it cannot happen: the URI is rebuilt from a URI that already parsed.
        None => tracing::warn!(
            uri = %parts.uri,
            "Failed to drop the share link session key from a request URI",
        ),
    }

    add_baggage(&mut parts.headers, &param.value);

    Some(ShareLinkAction::SetCookie(join_cookie(&param.value)?))
}

/// [`inspect`] for a request that carries no share link param.
///
/// A cookie only carries a session the viewer already joined, so there is nothing to clean out of
/// the URL and no cookie to hand back - unless the session ended in the meantime.
fn inspect_cookie(parts: &mut Parts, is_active: impl Fn(&str) -> bool) -> Option<ShareLinkAction> {
    let key = cookie_key(&parts.headers)?;

    if is_active(&key).not() {
        return Some(ShareLinkAction::SetCookie(CLEAR_COOKIE.clone()));
    }

    add_baggage(&mut parts.headers, &key);

    None
}

/// A share link's session key, with the URL the app should see once the key is gone.
struct KeyParam {
    value: String,
    /// The request's path and query without the key. Always starts with `/`, so it is safe to use
    /// as a redirect target: it can only point back at the app the request was already going to.
    without_key: String,
}

/// Takes the `mirrord-session` param out of `uri`, in one pass over the query.
///
/// The params that stay keep their exact bytes, which is why the query is cut on `&` rather than
/// parsed and re-serialized: a urlencoded serializer normalizes what it writes back (`a+b` turns
/// into `a%20b`), and the app may have signed those bytes or keyed a cache on them.
///
/// An empty value (`?mirrord-session=`) is not a key, and leaves the URL alone.
fn key_param(uri: &Uri) -> Option<KeyParam> {
    let mut value = None;
    let mut kept = Vec::new();

    for pair in uri.query().into_iter().flat_map(|query| query.split('&')) {
        let name = pair.split_once('=').map_or(pair, |(name, _)| name);
        if name != SESSION_KEY {
            kept.push(pair);
            continue;
        }

        // Every `mirrord-session` param is dropped, but only the first one with a value is a key.
        if value.is_none() {
            value = session_key_of(pair);
        }
    }

    let value = value?;

    let mut without_key = match uri.path() {
        "" => "/".to_owned(),
        path => path.to_owned(),
    };
    if kept.is_empty().not() {
        without_key.push('?');
        without_key.push_str(&kept.join("&"));
    }

    Some(KeyParam { value, without_key })
}

/// Finds the session key among the request's cookies.
fn cookie_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| session_key_of(pair.trim()))
}

/// Returns the value of a `name=value` pair when it names the session key and is not empty.
fn session_key_of(pair: &str) -> Option<String> {
    let (name, value) = pair.split_once('=')?;

    (name == SESSION_KEY && value.is_empty().not()).then(|| value.to_owned())
}

/// Rebuilds `uri` with a different path and query, keeping the scheme and authority that HTTP/2
/// requests carry.
fn replace_path_and_query(uri: &Uri, path_and_query: &str) -> Option<Uri> {
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_str(path_and_query).ok()?);

    Uri::from_parts(parts).ok()
}

/// Adds `mirrord-session=<key>` to the request's baggage.
///
/// Existing baggage is merged into the single comma-separated list the format asks for. A value
/// that is not valid UTF-8 is not baggage and is dropped.
fn add_baggage(headers: &mut HeaderMap, key: &str) {
    let entry = format!("{SESSION_KEY}={key}");

    let mut baggage = headers
        .get_all(BAGGAGE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join(",");

    // Apps forward baggage (and some forward cookies) to the services they call, so a request
    // may arrive already carrying this exact entry from an earlier hop. Appending it again on
    // every hop would grow the header along the call chain.
    if baggage.split(',').any(|existing| existing.trim() == entry) {
        return;
    }

    if baggage.is_empty().not() {
        baggage.push(',');
    }
    baggage.push_str(&entry);

    match HeaderValue::from_str(&baggage) {
        Ok(value) => {
            headers.insert(BAGGAGE, value);
        }
        Err(error) => tracing::warn!(
            %error,
            "Share link session key cannot be sent as a baggage header, \
            the request will not join the session",
        ),
    }
}

/// Builds the cookie that keeps the viewer in the joined session.
fn join_cookie(key: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!("{SESSION_KEY}={key}{JOIN_COOKIE_ATTRIBUTES}"))
        .inspect_err(|error| {
            tracing::warn!(
                %error,
                "Share link session key cannot be sent as a cookie, \
                the viewer will need the query param on every request",
            )
        })
        .ok()
}

/// Makes the response for a share link whose session is gone.
///
/// The viewer asked for a real page of a real app, so this is a `200` interstitial rather than an
/// error: after the countdown it continues to `continue_to`, which is the same URL without the
/// session key, and the app answers it normally.
///
/// It also clears the session cookie. A viewer who is already in a session and opens a share link
/// for one that has ended is leaving the session they were in - the param always wins - so keeping
/// the cookie would put the countdown's own follow-up request back into that older session.
pub fn session_ended_response(version: Version, continue_to: &str) -> BoxResponse {
    let html = SESSION_ENDED_PAGE
        .replace("{{SECONDS}}", &SESSION_ENDED_COUNTDOWN_SECS.to_string())
        .replace("{{URL}}", &escape_html_attribute(continue_to));

    let body = Full::new(Bytes::from(html))
        .map_err(|never: Infallible| match never {})
        .boxed();

    let mut response = Response::new(body);
    *response.version_mut() = version;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HTML_CONTENT_TYPE);
    response.headers_mut().insert(CACHE_CONTROL, NO_STORE);
    response
        .headers_mut()
        .insert(SET_COOKIE, CLEAR_COOKIE.clone());

    response
}

/// Escapes a string for use inside a double-quoted HTML attribute.
///
/// The URL comes from the request, so whoever crafts a share link chooses it. Without escaping,
/// they could close the attribute and inject markup into a page served from the app's origin.
/// This renders one interstitial page, so the chained allocations cost nothing; `&` goes first
/// so the other replacements are not escaped again.
fn escape_html_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Makes the response's `Set-Cookie` header arrive with whatever response this request gets.
///
/// The response is produced elsewhere - by the stealing client, or by the app itself when the
/// request is passed through - and both answer through this one channel, so wrapping the channel
/// is the single place that covers every path.
///
/// Returns the sender to use in place of `response_tx`.
pub fn with_set_cookie(
    response_tx: oneshot::Sender<BoxResponse>,
    set_cookie: HeaderValue,
) -> oneshot::Sender<BoxResponse> {
    let (tx, rx) = oneshot::channel::<BoxResponse>();

    tokio::spawn(async move {
        // Dropping `response_tx` without a response is how the request is failed with a 502,
        // so a lost response needs no handling here.
        let Ok(mut response) = rx.await else {
            return;
        };

        response.headers_mut().append(SET_COOKIE, set_cookie);
        let _ = response_tx.send(response);
    });

    tx
}

/// The [session-ended page](session_ended_response), with `{{SECONDS}}` and `{{URL}}`
/// placeholders filled in per request. `{{URL}}` is always [escaped](escape_html_attribute).
const SESSION_ENDED_PAGE: &str = include_str!("session_ended.html");

#[cfg(test)]
mod test {
    use http_body_util::BodyExt;
    use hyper::Request;
    use rstest::rstest;
    use tokio::sync::oneshot;

    use super::*;

    /// Builds the parts of a request to `uri`, with an optional `Cookie` header.
    fn request(uri: &str, cookie: Option<&str>) -> Parts {
        let mut builder = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            builder = builder.header(COOKIE, cookie);
        }

        builder.body(()).unwrap().into_parts().0
    }

    /// Runs [`inspect`] with `active` as the set of live session keys.
    fn inspect_with(parts: &mut Parts, active: &[&str]) -> Option<ShareLinkAction> {
        inspect(parts, |key| active.contains(&key))
    }

    fn baggage_of(parts: &Parts) -> Option<&str> {
        parts.headers.get(BAGGAGE).map(|value| {
            value
                .to_str()
                .expect("tests only ever set valid UTF-8 baggage")
        })
    }

    /// A request with no session key anywhere must come out exactly as it went in - this is every
    /// request of every app that does not use share links.
    #[test]
    fn request_without_a_session_key_is_untouched() {
        let mut parts = request("/orders?page=2", Some("theme=dark"));

        assert_eq!(inspect_with(&mut parts, &["live"]), None);
        assert_eq!(parts.uri, "/orders?page=2");
        assert_eq!(baggage_of(&parts), None);
    }

    #[rstest]
    #[case::only_param("/orders?mirrord-session=live", "/orders")]
    #[case::first_param("/orders?mirrord-session=live&page=2", "/orders?page=2")]
    #[case::last_param("/orders?page=2&mirrord-session=live", "/orders?page=2")]
    #[case::between_params(
        "/orders?page=2&mirrord-session=live&sort=asc",
        "/orders?page=2&sort=asc"
    )]
    #[case::repeated_param("/?mirrord-session=live&mirrord-session=live", "/")]
    // The app may have signed its params or keyed a cache on them, so the ones that stay must
    // arrive byte for byte - no re-encoding of `+`, `%20` or anything else.
    #[case::other_params_keep_their_bytes(
        "/s?q=red+shoes&tag=%7Enew&mirrord-session=live",
        "/s?q=red+shoes&tag=%7Enew"
    )]
    // HTTP/2 requests carry the scheme and authority in the URI; both must survive.
    #[case::absolute_uri(
        "http://shop.dev/orders?mirrord-session=live",
        "http://shop.dev/orders"
    )]
    #[test]
    fn joining_via_param_adds_baggage_and_drops_the_param(
        #[case] uri: &str,
        #[case] expected: &str,
    ) {
        let mut parts = request(uri, None);

        let action = inspect_with(&mut parts, &["live"]);

        assert_eq!(parts.uri, expected);
        assert_eq!(baggage_of(&parts), Some("mirrord-session=live"));
        assert_eq!(
            action,
            Some(ShareLinkAction::SetCookie(HeaderValue::from_static(
                "mirrord-session=live; Path=/; HttpOnly; SameSite=Lax"
            )))
        );
    }

    /// Baggage usually already carries trace context, which the app and the services behind it
    /// need. Joining a session must add to that list, not replace it.
    #[test]
    fn existing_baggage_is_kept() {
        let mut parts = request("/?mirrord-session=live", None);
        parts.headers.insert(BAGGAGE, "userId=7".parse().unwrap());

        inspect_with(&mut parts, &["live"]);

        assert_eq!(baggage_of(&parts), Some("userId=7,mirrord-session=live"));
    }

    /// Apps forward baggage (and some forward cookies) downstream, so a request can arrive
    /// already carrying the entry from an earlier hop. Appending again on every hop would grow
    /// the header along the call chain.
    #[test]
    fn baggage_entry_is_not_duplicated() {
        let mut parts = request("/", Some("mirrord-session=live"));
        parts
            .headers
            .insert(BAGGAGE, "userId=7,mirrord-session=live".parse().unwrap());

        assert_eq!(inspect_with(&mut parts, &["live"]), None);
        assert_eq!(baggage_of(&parts), Some("userId=7,mirrord-session=live"));
    }

    /// The cookie carries the session for every request after the first, so those requests have
    /// nothing to rewrite in the URL and need no new cookie.
    #[test]
    fn joining_via_cookie_only_adds_baggage() {
        let mut parts = request("/orders?page=2", Some("theme=dark; mirrord-session=live"));

        assert_eq!(inspect_with(&mut parts, &["live"]), None);
        assert_eq!(parts.uri, "/orders?page=2");
        assert_eq!(baggage_of(&parts), Some("mirrord-session=live"));
    }

    /// Opening a fresh share link while already in another session has to switch sessions,
    /// otherwise the viewer would be stuck in the first one for as long as the browser is open.
    #[test]
    fn param_wins_over_cookie() {
        let mut parts = request("/?mirrord-session=new", Some("mirrord-session=old"));

        inspect_with(&mut parts, &["old", "new"]);

        assert_eq!(baggage_of(&parts), Some("mirrord-session=new"));
    }

    /// A link that outlived its session: the viewer gets the page, and it continues to the same
    /// URL without the key, which the app answers normally.
    #[test]
    fn param_for_a_dead_session_ends_the_request() {
        let mut parts = request("/orders?mirrord-session=gone&page=2", None);

        let action = inspect_with(&mut parts, &[]);

        assert_eq!(
            action,
            Some(ShareLinkAction::SessionEnded {
                continue_to: "/orders?page=2".to_owned(),
            })
        );
        assert_eq!(baggage_of(&parts), None);
    }

    /// A viewer in session `live` opens a share link for `gone`. The param wins, so they are
    /// leaving `live` either way: if the page kept their cookie, its own follow-up request would
    /// carry `live` and drop them back into the session the new link told them to leave.
    #[test]
    fn a_dead_link_leaves_the_session_the_viewer_was_in() {
        let mut parts = request("/orders?mirrord-session=gone", Some("mirrord-session=live"));

        assert_eq!(
            inspect_with(&mut parts, &["live"]),
            Some(ShareLinkAction::SessionEnded {
                continue_to: "/orders".to_owned(),
            }),
        );

        let response = session_ended_response(parts.version, "/orders");

        assert_eq!(
            response.headers().get(SET_COOKIE),
            Some(&HeaderValue::from_static(
                "mirrord-session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
            )),
        );
    }

    /// The session ended while the viewer was browsing. Clearing the cookie is what stops every
    /// following request from asking about a key that is never coming back.
    #[test]
    fn cookie_for_a_dead_session_is_cleared() {
        let mut parts = request("/orders", Some("mirrord-session=gone"));

        assert_eq!(
            inspect_with(&mut parts, &[]),
            Some(ShareLinkAction::SetCookie(HeaderValue::from_static(
                "mirrord-session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
            ))),
        );
        assert_eq!(parts.uri, "/orders");
        assert_eq!(baggage_of(&parts), None);
    }

    #[rstest]
    #[case::empty_param("/?mirrord-session=", None)]
    #[case::empty_cookie("/", Some("mirrord-session="))]
    #[case::similar_param("/?xmirrord-session=live", None)]
    #[case::similar_cookie("/", Some("mirrord-sessionx=live"))]
    #[test]
    fn only_a_real_session_key_counts(#[case] uri: &str, #[case] cookie: Option<&str>) {
        let mut parts = request(uri, cookie);

        assert_eq!(inspect_with(&mut parts, &["live"]), None);
        assert_eq!(baggage_of(&parts), None);
    }

    /// Whoever crafts a share link picks the URL, and the page that shows it is served from the
    /// app's own origin. Escaping is what keeps a crafted link from running script there.
    #[tokio::test]
    async fn session_ended_page_escapes_the_url() {
        let response =
            session_ended_response(Version::HTTP_11, "/\"><script>alert(1)</script>?a=1&b=2");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = String::from_utf8(body.to_vec()).unwrap();

        assert!(body.contains("/&quot;&gt;&lt;script&gt;"), "{body}");
        assert!(body.contains("?a=1&amp;b=2"), "{body}");
        assert!(body.contains("<script>alert(1)").not(), "{body}");
    }

    /// An operator that dies cannot send `RemoveKey`. Keys must die with the connection that
    /// registered them, otherwise a long-lived agent keeps joining viewers to sessions that are
    /// gone, and its key set grows forever.
    #[test]
    fn keys_die_with_the_client_connection() {
        let shared = ShareLinkKeys::default();

        let mut client = shared.for_client();
        client.update(ShareLinkRequest::RegisterKey("live".to_owned()));
        assert!(shared.is_active("live"));

        drop(client);
        assert!(shared.is_active("live").not());
    }

    /// Removing a key and then losing the connection must not touch other keys, and must not
    /// release the removed key twice.
    #[test]
    fn remove_key_takes_effect_immediately() {
        let shared = ShareLinkKeys::default();

        let mut client = shared.for_client();
        client.update(ShareLinkRequest::RegisterKey("gone".to_owned()));
        client.update(ShareLinkRequest::RegisterKey("live".to_owned()));
        client.update(ShareLinkRequest::RemoveKey("gone".to_owned()));

        assert!(shared.is_active("gone").not());
        assert!(shared.is_active("live"));

        drop(client);
        assert!(shared.is_active("live").not());
    }

    /// Two connections holding the same key: one of them going away must not kick out the
    /// viewers the other one still serves.
    #[test]
    fn key_stays_while_another_client_holds_it() {
        let shared = ShareLinkKeys::default();

        let mut first = shared.for_client();
        let mut second = shared.for_client();
        first.update(ShareLinkRequest::RegisterKey("live".to_owned()));
        second.update(ShareLinkRequest::RegisterKey("live".to_owned()));

        drop(first);
        assert!(shared.is_active("live"));

        drop(second);
        assert!(shared.is_active("live").not());
    }

    /// Registering the same key twice over one connection is one registration, so it cannot
    /// leak a count that outlives the connection.
    #[test]
    fn duplicate_register_from_one_client_does_not_leak() {
        let shared = ShareLinkKeys::default();

        let mut client = shared.for_client();
        client.update(ShareLinkRequest::RegisterKey("live".to_owned()));
        client.update(ShareLinkRequest::RegisterKey("live".to_owned()));

        drop(client);
        assert!(shared.is_active("live").not());
    }

    /// The response comes from the stealing client or from the app, and neither knows about
    /// cookies, so the cookie has to be added on the way back out.
    #[tokio::test]
    async fn set_cookie_reaches_the_response() {
        let (response_tx, response_rx) = oneshot::channel();
        let wrapped = with_set_cookie(response_tx, CLEAR_COOKIE.clone());

        let mut response = Response::new(
            Full::new(Bytes::new())
                .map_err(|never: Infallible| match never {})
                .boxed(),
        );
        response
            .headers_mut()
            .append(SET_COOKIE, HeaderValue::from_static("theme=dark"));
        wrapped.send(response).unwrap();

        let response = response_rx.await.unwrap();
        let cookies = response
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .collect::<Vec<_>>();

        assert_eq!(
            cookies,
            [&HeaderValue::from_static("theme=dark"), &*CLEAR_COOKIE],
        );
    }
}
