/// JWT auth for the peer boundary (:7241).
///
/// Tokens are short-lived (1 hour), HS256, issued by the calling gateway.
/// Claims carry the issuer (gateway name) and optional scope.
///
/// The LAN side (:7240) has no auth — LAN is trusted.

use anyhow::{bail, Context, Result};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use crate::util::now_secs;

const TOKEN_TTL_SECS: u64 = 3600; // 1 hour

// ── Claims ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerClaims {
    pub iss: String,    // issuing gateway name
    pub sub: String,    // subject: target gateway name (or "*" for broadcast)
    pub exp: u64,       // unix timestamp
    pub iat: u64,       // issued at
    pub scope: String,  // "peer" | "sync" | "delegate"
}

impl PeerClaims {
    pub fn new(issuer: &str, subject: &str, scope: &str) -> Self {
        let now = now_secs() as u64;
        Self {
            iss:   issuer.to_owned(),
            sub:   subject.to_owned(),
            exp:   now + TOKEN_TTL_SECS,
            iat:   now,
            scope: scope.to_owned(),
        }
    }

    pub fn is_expired(&self) -> bool {
        now_secs() as u64 > self.exp
    }
}

// ── JwtAuth ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct JwtAuth {
    encoding: EncodingKey,
    decoding: DecodingKey,
    issuer:   String,
}

impl JwtAuth {
    pub fn new(secret: &[u8], issuer: impl Into<String>) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            issuer:   issuer.into(),
        }
    }

    /// Issue a short-lived token for an outbound peer call.
    pub fn issue(&self, target_gateway: &str, scope: &str) -> Result<String> {
        let claims = PeerClaims::new(&self.issuer, target_gateway, scope);
        encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .context("issue JWT")
    }

    /// Validate an inbound JWT. Returns claims if valid.
    pub fn validate(&self, token: &str) -> Result<PeerClaims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;

        let data = decode::<PeerClaims>(token, &self.decoding, &validation)
            .context("invalid JWT")?;

        let claims = data.claims;
        if claims.is_expired() {
            bail!("token expired");
        }
        Ok(claims)
    }

    /// Validate and check that subject matches the expected target (us).
    pub fn validate_for(&self, token: &str, expected_sub: &str) -> Result<PeerClaims> {
        let claims = self.validate(token)?;
        if claims.sub != "*" && claims.sub != expected_sub {
            bail!("token not issued for this gateway (sub={}, expected={})",
                  claims.sub, expected_sub);
        }
        Ok(claims)
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn auth(name: &str) -> JwtAuth {
        JwtAuth::new(b"test-secret-1234", name)
    }

    #[test]
    fn issue_and_validate() {
        let a = auth("site-a");
        let token = a.issue("site-b", "peer").unwrap();
        let claims = a.validate(&token).unwrap();
        assert_eq!(claims.iss, "site-a");
        assert_eq!(claims.sub, "site-b");
        assert_eq!(claims.scope, "peer");
    }

    #[test]
    fn validate_for_correct_sub() {
        let a = auth("site-a");
        let token = a.issue("site-b", "sync").unwrap();
        let claims = a.validate_for(&token, "site-b").unwrap();
        assert_eq!(claims.scope, "sync");
    }

    #[test]
    fn validate_for_wrong_sub_rejected() {
        let a = auth("site-a");
        let token = a.issue("site-b", "sync").unwrap();
        let err = a.validate_for(&token, "site-c").unwrap_err();
        assert!(err.to_string().contains("not issued for this gateway"));
    }

    #[test]
    fn wildcard_sub_accepted_for_any() {
        let a = auth("site-a");
        let mut claims = PeerClaims::new("site-a", "*", "peer");
        let token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(b"test-secret-1234"),
        ).unwrap();
        a.validate_for(&token, "anyone").unwrap();
    }

    #[test]
    fn different_secret_rejected() {
        let a = auth("site-a");
        let b = JwtAuth::new(b"different-secret", "site-b");
        let token = a.issue("site-b", "peer").unwrap();
        assert!(b.validate(&token).is_err());
    }
}
