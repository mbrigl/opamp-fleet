//! The parameters and the headers of the four lists that grow with the fleet
//! (ADR-0031
//! clauses 3, 4 and 6): `limit` and `cursor`, a `400` for a parameter a route does not name, and
//! the `Link` and `X-Total-Count` headers of the answer.

use std::collections::BTreeMap;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use base64::Engine as _;
use serde::Deserialize;

use crate::listing::{self, Position, MAX_LIMIT, WHOLE_BOUND};

/// What a cursor holds: a position, and the digest of the sort and filters it was made for.
#[derive(Serialize, Deserialize)]
struct CursorBody {
    p: Position,
    d: String,
}

/// The opaque cursor of the page after `position` (clause 4).
#[must_use]
pub(super) fn encode_cursor(position: &Position, digest: &str) -> String {
    let body = CursorBody {
        p: position.clone(),
        d: digest.to_string(),
    };
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&body).expect("a cursor serializes"))
}

/// The position a cursor names, `None` when it does not parse or was made for another sort or
/// other filters than those `digest` stands for.
#[must_use]
pub(super) fn decode_cursor(cursor: &str, digest: &str) -> Option<Position> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()?;
    let body: CursorBody = serde_json::from_slice(&bytes).ok()?;
    // A NUL names no position a store keeps: a cursor holding one was not made by this Server.
    let nul = body.p.tiebreak.contains('\0')
        || matches!(&body.p.key, listing::Key::Text(text) if text.contains('\0'));
    (body.d == digest && !nul).then_some(body.p)
}

/// What a request asks of a list.
pub(super) struct Asked {
    /// The parameters as they arrived, `cursor` left out: what the next page's link repeats.
    pairs: Vec<(String, String)>,
    /// Every parameter but `limit` and `cursor`, by name.
    pub(super) params: BTreeMap<String, String>,
    /// `None` for the whole list.
    pub(super) limit: Option<usize>,
    cursor: Option<String>,
}

/// A `400` naming what is wrong.
pub(super) fn refused(message: impl Into<String>) -> super::Refusal {
    (StatusCode::BAD_REQUEST, message.into())
}

/// The query string's pairs, decoded as a form encodes them; `None` where one does not decode.
fn pairs(raw: &str) -> Option<Vec<(String, String)>> {
    fn decode(text: &str) -> Option<String> {
        let bytes = text.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'+' => out.push(b' '),
                b'%' => {
                    let hex = bytes.get(i + 1..i + 3)?;
                    if !hex.iter().all(u8::is_ascii_hexdigit) {
                        return None;
                    }
                    out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
                    i += 2;
                }
                byte => out.push(byte),
            }
            i += 1;
        }
        String::from_utf8(out).ok()
    }
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((decode(name)?, decode(value)?))
        })
        .collect()
}

/// Text as a query string carries it: everything but the unreserved characters escaped.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

impl Asked {
    /// The parameters of `raw`, each one `names` holds or `limit` or `cursor`, each at most once.
    pub(super) fn parse(raw: Option<&str>, names: &[&str]) -> Result<Self, super::Refusal> {
        let pairs = pairs(raw.unwrap_or_default())
            .ok_or_else(|| refused("the query string: a pair does not decode"))?;
        let mut params = BTreeMap::new();
        let (mut limit, mut cursor) = (None, None);
        for (name, value) in &pairs {
            // No store holds a NUL in a text it searches or sorts by.
            if value.contains('\0') {
                return Err(refused(format!("{name}: holds a NUL character")));
            }
            let slot = match name.as_str() {
                "limit" => &mut limit,
                "cursor" => &mut cursor,
                known if names.contains(&known) => {
                    if params.insert(name.clone(), value.clone()).is_some() {
                        return Err(refused(format!("{name}: named twice")));
                    }
                    continue;
                }
                _ => {
                    return Err(refused(format!(
                        "{name}: this route takes no such parameter"
                    )))
                }
            };
            if slot.replace(value.clone()).is_some() {
                return Err(refused(format!("{name}: named twice")));
            }
        }
        let limit = match (limit, &cursor) {
            (Some(limit), _) => Some(
                Some(limit.as_str())
                    .filter(|limit| !limit.is_empty() && limit.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|limit| limit.parse::<u64>().ok())
                    .filter(|limit| *limit > 0)
                    .map(|limit| usize::try_from(limit).unwrap_or(MAX_LIMIT).min(MAX_LIMIT))
                    .ok_or_else(|| refused(format!("limit: {limit:?} is not 1 to {MAX_LIMIT}")))?,
            ),
            // A cursor names a page, and a page holds at most the most a page holds.
            (None, Some(_)) => Some(MAX_LIMIT),
            (None, None) => None,
        };
        Ok(Asked {
            pairs: pairs
                .into_iter()
                .filter(|(name, _)| name != "cursor")
                .collect(),
            params,
            limit,
            cursor,
        })
    }

    /// The digest a cursor of this request names: its parameters but `limit` and `cursor`.
    pub(super) fn digest(&self) -> String {
        let named: Vec<(&str, &str)> = self
            .params
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        listing::digest(&named)
    }

    /// The position the cursor names, its key a text where `text` says so and an integer
    /// otherwise, or the `400` that says it names none for this request.
    pub(super) fn after_keyed(&self, text: bool) -> Result<Option<Position>, super::Refusal> {
        let after = self.after()?;
        if after
            .as_ref()
            .is_some_and(|after| matches!(after.key, listing::Key::Text(_)) != text)
        {
            return Err(refused("cursor: it was made for another sort or filters"));
        }
        Ok(after)
    }

    /// The position the cursor names, or the `400` that says it names none for this request.
    pub(super) fn after(&self) -> Result<Option<Position>, super::Refusal> {
        self.cursor
            .as_ref()
            .map(|cursor| {
                decode_cursor(cursor, &self.digest()).ok_or_else(|| {
                    refused("cursor: it does not parse, or was made for another sort or filters")
                })
            })
            .transpose()
    }

    /// How many entries to read: the page's, or the bound of a whole answer and one more, which
    /// tells a longer list.
    pub(super) fn reading(&self) -> usize {
        self.limit.unwrap_or(WHOLE_BOUND)
    }

    /// The answer: the entries as a JSON array, `X-Total-Count`, and a `Link` to the next page
    /// where one follows. A whole answer over the bound is a `400` naming `limit`.
    pub(super) fn answer<T: Serialize>(
        &self,
        route: &str,
        entries: Vec<T>,
        total: usize,
        next: Option<Position>,
    ) -> Response {
        if self.limit.is_none() && total > WHOLE_BOUND {
            return super::error(
                StatusCode::BAD_REQUEST,
                format!(
                    "limit: {total} entries match, more than the {WHOLE_BOUND} answered whole; \
                 ask for a page with limit and follow its Link"
                ),
            );
        }
        let mut response = Json(entries).into_response();
        let headers = response.headers_mut();
        headers.insert(
            "x-total-count",
            HeaderValue::from_str(&total.to_string()).expect("a number is a header value"),
        );
        if let (Some(next), Some(_)) = (next, self.limit) {
            let mut query: Vec<String> = self
                .pairs
                .iter()
                .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
                .collect();
            query.push(format!(
                "cursor={}",
                encode(&encode_cursor(&next, &self.digest()))
            ));
            let link = format!("<{route}?{}>; rel=\"next\"", query.join("&"));
            headers.insert(
                header::LINK,
                HeaderValue::from_str(&link).expect("an escaped link is a header value"),
            );
        }
        response
    }

    /// A boolean parameter, `true` or `false`.
    pub(super) fn flag(&self, name: &str) -> Result<Option<bool>, super::Refusal> {
        self.params
            .get(name)
            .map(|value| match value.as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                other => Err(refused(format!(
                    "{name}: {other:?} is neither true nor false"
                ))),
            })
            .transpose()
    }

    /// `q`, lowercased; `None` where it is absent or empty.
    pub(super) fn q(&self) -> Option<String> {
        self.params
            .get("q")
            .filter(|q| !q.is_empty())
            .map(|q| q.to_lowercase())
    }
}

/// Cuts a page of a list held in memory: the entries that pass, in their order.
pub(super) fn cut<T>(
    asked: &Asked,
    after: Option<&Position>,
    entries: Vec<(Position, T)>,
) -> listing::Paged<T> {
    listing::page(entries, false, after, asked.reading())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cursor comes back as the position it was made of, and only for the sort and filters it
    /// was made for; one that does not parse names nothing.
    /// Verifies: ADR-0031
    #[test]
    fn a_cursor_names_its_position_for_its_own_sort_and_filters_alone() {
        let made_for = listing::digest(&[("sort", "name"), ("q", "edge")]);
        let position = Position {
            key: listing::Key::Text("web-1".to_string()),
            tiebreak: "uid".to_string(),
        };
        let cursor = encode_cursor(&position, &made_for);
        assert_eq!(decode_cursor(&cursor, &made_for), Some(position));
        assert_eq!(
            decode_cursor(
                &cursor,
                &listing::digest(&[("sort", "name"), ("q", "core")])
            ),
            None
        );
        assert_eq!(decode_cursor("not a cursor", &made_for), None);
    }

    /// A query string decodes as a form encodes it, and what the link repeats encodes back.
    #[test]
    fn a_query_string_decodes_and_encodes_back() {
        assert_eq!(
            pairs("q=a+b%3Dc&selector=tier%3Dedge"),
            Some(vec![
                ("q".to_string(), "a b=c".to_string()),
                ("selector".to_string(), "tier=edge".to_string()),
            ])
        );
        assert_eq!(pairs("q=%zz"), None);
        assert_eq!(
            pairs(&format!("q={}", encode("a b=c&d"))),
            Some(vec![("q".to_string(), "a b=c&d".to_string())])
        );
    }
}
