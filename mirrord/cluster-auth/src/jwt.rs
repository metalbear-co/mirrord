//! Expiry and refresh timing of JWT bearer tokens, such as Kubernetes service account tokens.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;

/// Maximum refresh interval / fallback if JWT parsing fails (45 minutes).
pub const MAX_REFRESH_INTERVAL: Duration = Duration::from_secs(45 * 60);

/// Default token expiration when requesting new tokens (1 hour).
pub const DEFAULT_TOKEN_EXPIRATION_SECONDS: i64 = 3600;

/// JWT claims we care about.
#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
struct JwtClaims {
    /// Expiration time as Unix timestamp.
    exp: u64,
    /// Issued at time as Unix timestamp.
    #[serde(default)]
    iat: Option<u64>,
}

/// Parse JWT claims from a token (no validation, just extraction).
fn parse_jwt_claims(token: &str) -> Option<JwtClaims> {
    let payload = token.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice(&decoded).ok()
}

/// Parse token expiry time. Returns None if parsing fails.
pub fn parse_token_expiry(token: &str) -> Option<SystemTime> {
    let claims = parse_jwt_claims(token)?;
    Some(UNIX_EPOCH + Duration::from_secs(claims.exp))
}

/// Compute when to refresh a token - the halfway point of its lifetime.
/// If we can parse iat+exp, halfway = iat + (exp-iat)/2.
/// If we only have exp, halfway = now + (exp-now)/2 (less accurate but safe).
/// Returns None if the token is already expired or unparseable.
pub fn compute_refresh_at(token: &str) -> Option<SystemTime> {
    let claims = parse_jwt_claims(token)?;
    let expiry = UNIX_EPOCH + Duration::from_secs(claims.exp);

    let halfway = if let Some(iat) = claims.iat {
        let issued_at = UNIX_EPOCH + Duration::from_secs(iat);
        let lifetime = expiry.duration_since(issued_at).ok()?;
        issued_at + lifetime / 2
    } else {
        let remaining = expiry.duration_since(SystemTime::now()).ok()?;
        SystemTime::now() + remaining / 2
    };

    Some(halfway)
}

/// Time until we should refresh. Returns None if refresh is needed now
/// (past the halfway point or unparseable token).
pub fn time_until_refresh(token: &str) -> Option<Duration> {
    let refresh_at = compute_refresh_at(token)?;
    let remaining = refresh_at.duration_since(SystemTime::now()).ok()?;
    Some(remaining.min(MAX_REFRESH_INTERVAL))
}

/// Check if a token needs refresh (expired or past its halfway point).
pub fn needs_refresh(token: &str) -> bool {
    time_until_refresh(token).is_none()
}

/// Parse token's original lifetime (exp - iat) in seconds.
pub fn parse_token_lifetime(token: &str) -> i64 {
    parse_jwt_claims(token)
        .and_then(|c| c.iat.map(|iat| c.exp.saturating_sub(iat) as i64))
        .filter(|&lifetime| lifetime > 0)
        .unwrap_or(DEFAULT_TOKEN_EXPIRATION_SECONDS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(claims: &JwtClaims) -> String {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
        format!("header.{payload}.signature")
    }

    fn unix(time: SystemTime) -> u64 {
        time.duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    #[test]
    fn refresh_is_due_halfway_through_the_lifetime() {
        let issued_at = unix(SystemTime::now()) - 600;
        let token = token(&JwtClaims {
            exp: issued_at + 3600,
            iat: Some(issued_at),
        });

        assert_eq!(
            compute_refresh_at(&token),
            Some(UNIX_EPOCH + Duration::from_secs(issued_at + 1800))
        );
        assert!(!needs_refresh(&token));
        assert_eq!(parse_token_lifetime(&token), 3600);
    }

    #[test]
    fn token_past_its_halfway_point_needs_refresh() {
        let issued_at = unix(SystemTime::now()) - 3000;
        let token = token(&JwtClaims {
            exp: issued_at + 3600,
            iat: Some(issued_at),
        });

        assert!(needs_refresh(&token));
    }

    #[test]
    fn unparseable_token_needs_refresh_and_gets_the_default_lifetime() {
        assert_eq!(parse_token_expiry("not-a-jwt"), None);
        assert!(needs_refresh("not-a-jwt"));
        assert_eq!(
            parse_token_lifetime("not-a-jwt"),
            DEFAULT_TOKEN_EXPIRATION_SECONDS
        );
    }
}
