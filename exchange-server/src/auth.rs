use std::{collections::BTreeMap, env, error::Error, fmt};

use axum::http::{HeaderMap, header::AUTHORIZATION};

pub const AUTH_TOKENS_ENV: &str = "MARKETFORGE_AUTH_TOKENS_JSON";
pub const USER_ID_HEADER: &str = "x-user-id";
pub const DEFAULT_USER_ID: &str = "local-user";

#[derive(Clone)]
pub enum AuthPolicy {
    LocalDevelopment,
    BearerTokens(BTreeMap<String, String>),
}

impl fmt::Debug for AuthPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalDevelopment => f.write_str("LocalDevelopment"),
            Self::BearerTokens(tokens) => f
                .debug_struct("BearerTokens")
                .field("configured_token_count", &tokens.len())
                .finish(),
        }
    }
}

impl AuthPolicy {
    pub fn local_development() -> Self {
        Self::LocalDevelopment
    }

    pub fn from_env() -> Result<Self, AuthError> {
        match env::var(AUTH_TOKENS_ENV) {
            Ok(raw) => Self::from_token_json(&raw),
            Err(env::VarError::NotPresent) => Ok(Self::LocalDevelopment),
            Err(error) => Err(AuthError::Configuration(error.to_string())),
        }
    }

    pub fn from_token_json(raw: &str) -> Result<Self, AuthError> {
        let tokens: BTreeMap<String, String> = serde_json::from_str(raw)
            .map_err(|error| AuthError::Configuration(error.to_string()))?;
        if tokens.is_empty() {
            return Err(AuthError::Configuration(format!(
                "{AUTH_TOKENS_ENV} must contain at least one bearer token"
            )));
        }
        for (token, user_id) in &tokens {
            if token.trim().is_empty() {
                return Err(AuthError::Configuration(
                    "bearer tokens must not be empty".to_string(),
                ));
            }
            if user_id.trim().is_empty() {
                return Err(AuthError::Configuration(
                    "bearer-token user ids must not be empty".to_string(),
                ));
            }
        }
        Ok(Self::BearerTokens(tokens))
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Result<String, AuthError> {
        match self {
            Self::LocalDevelopment => local_user_id(headers),
            Self::BearerTokens(tokens) => {
                let header = headers
                    .get(AUTHORIZATION)
                    .ok_or(AuthError::MissingCredentials)?
                    .to_str()
                    .map_err(|_| AuthError::InvalidCredentials)?;
                let token = header
                    .strip_prefix("Bearer ")
                    .filter(|token| !token.is_empty())
                    .ok_or(AuthError::InvalidCredentials)?;
                tokens
                    .get(token)
                    .cloned()
                    .ok_or(AuthError::InvalidCredentials)
            }
        }
    }

    pub fn requires_bearer_token(&self) -> bool {
        matches!(self, Self::BearerTokens(_))
    }
}

fn local_user_id(headers: &HeaderMap) -> Result<String, AuthError> {
    match headers.get(USER_ID_HEADER) {
        Some(value) => {
            let user_id = value.to_str().map_err(|_| AuthError::InvalidUserId)?;
            let user_id = user_id.trim();
            if user_id.is_empty() {
                return Err(AuthError::InvalidUserId);
            }
            Ok(user_id.to_string())
        }
        None => Ok(DEFAULT_USER_ID.to_string()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthError {
    MissingCredentials,
    InvalidCredentials,
    InvalidUserId,
    Configuration(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCredentials => f.write_str("missing bearer credentials"),
            Self::InvalidCredentials => f.write_str("invalid bearer credentials"),
            Self::InvalidUserId => write!(f, "{USER_ID_HEADER} must contain a non-empty UTF-8 id"),
            Self::Configuration(error) => write!(f, "authentication configuration error: {error}"),
        }
    }
}

impl Error for AuthError {}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn bearer_mode_ignores_spoofed_user_header() {
        let policy = AuthPolicy::from_token_json(r#"{"secret-token":"alice"}"#).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(USER_ID_HEADER, HeaderValue::from_static("admin"));
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-token"),
        );

        assert_eq!(policy.authenticate(&headers), Ok("alice".to_string()));
    }

    #[test]
    fn bearer_mode_fails_closed_for_missing_or_unknown_tokens() {
        let policy = AuthPolicy::from_token_json(r#"{"secret-token":"alice"}"#).unwrap();
        assert_eq!(
            policy.authenticate(&HeaderMap::new()),
            Err(AuthError::MissingCredentials)
        );

        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert_eq!(
            policy.authenticate(&headers),
            Err(AuthError::InvalidCredentials)
        );
    }

    #[test]
    fn local_mode_preserves_loopback_development_identity() {
        let policy = AuthPolicy::local_development();
        assert_eq!(
            policy.authenticate(&HeaderMap::new()),
            Ok(DEFAULT_USER_ID.to_string())
        );

        let mut headers = HeaderMap::new();
        headers.insert(USER_ID_HEADER, HeaderValue::from_static("developer"));
        assert_eq!(policy.authenticate(&headers), Ok("developer".to_string()));
    }
}
