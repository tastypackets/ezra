use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::{Deserialize, Serialize};

pub const SESSION_COOKIE: &str = "session";

/// The manager password as an argon2id PHC string, never the password itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HashedPassword(String);

impl HashedPassword {
    pub fn from_password(password: &str) -> Result<Self, argon2::password_hash::Error> {
        let hash = Argon2::default().hash_password(password.as_bytes())?;
        Ok(Self(hash.to_string()))
    }

    pub fn matches(&self, password: &str) -> bool {
        PasswordHash::new(&self.0).is_ok_and(|parsed_hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed_hash)
                .is_ok()
        })
    }
}

/// A random token identifying one logged-in browser.
pub struct SessionToken(String);

impl SessionToken {
    fn generate() -> Result<Self, getrandom::Error> {
        let mut random_bytes = [0_u8; 32];
        getrandom::fill(&mut random_bytes)?;
        Ok(Self(
            random_bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        ))
    }

    pub fn into_cookie(self) -> Cookie<'static> {
        Cookie::build((SESSION_COOKIE, self.0))
            .path("/")
            .http_only(true)
            .secure(true)
            .same_site(SameSite::Strict)
            .build()
    }
}

/// Logged-in sessions, kept in memory: restarting the manager logs everyone out.
#[derive(Debug, Default)]
pub struct Sessions {
    tokens: Mutex<HashSet<String>>,
}

impl Sessions {
    pub fn start(&self) -> Result<SessionToken, getrandom::Error> {
        let token = SessionToken::generate()?;
        self.tokens().insert(token.0.clone());
        Ok(token)
    }

    pub fn is_active(&self, token: &str) -> bool {
        self.tokens().contains(token)
    }

    pub fn end(&self, token: &str) {
        self.tokens().remove(token);
    }

    fn tokens(&self) -> MutexGuard<'_, HashSet<String>> {
        self.tokens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_matches_only_its_own_hash() {
        let hashed = HashedPassword::from_password("correct horse").expect("password hashes");
        assert!(hashed.0.starts_with("$argon2id$"));
        assert!(hashed.matches("correct horse"));
        assert!(!hashed.matches("wrong horse"));
        assert!(!HashedPassword("not a hash".to_owned()).matches("correct horse"));
    }

    #[test]
    fn sessions_are_random_and_can_end() {
        let sessions = Sessions::default();
        let first = sessions.start().expect("session starts").0;
        let second = sessions.start().expect("session starts").0;
        assert_eq!(first.len(), 64);
        assert_ne!(first, second);
        assert!(sessions.is_active(&first));

        sessions.end(&first);
        assert!(!sessions.is_active(&first));
        assert!(sessions.is_active(&second));
    }

    #[test]
    fn session_cookie_is_locked_down() {
        let cookie = SessionToken("abc".to_owned()).into_cookie().to_string();
        for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(cookie.contains(attribute), "{cookie}");
        }
    }
}
