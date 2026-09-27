use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum_extra::extract::cookie::{Cookie, SameSite};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::bytes_ext::BytesExt;

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
        Ok(Self(random_bytes.to_hex()))
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
#[derive(Debug)]
pub struct Sessions {
    tokens: Mutex<HashSet<String>>,
    ended: watch::Sender<u64>,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            tokens: Mutex::default(),
            ended: watch::Sender::new(0),
        }
    }
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
        if self.tokens().remove(token) {
            self.note_ended();
        }
    }

    /// Ends every session but `kept`, and returns how many ended.
    pub fn end_others(&self, kept: &str) -> usize {
        let ended = {
            let mut tokens = self.tokens();
            let before = tokens.len();
            tokens.retain(|token| token == kept);
            before.saturating_sub(tokens.len())
        };
        if ended > 0 {
            self.note_ended();
        }
        ended
    }

    /// Resolves once `token` is no longer an active session.
    pub async fn until_ended(&self, token: &str) {
        let mut ended = self.ended.subscribe();
        while self.is_active(token) {
            if ended.changed().await.is_err() {
                return;
            }
        }
    }

    fn note_ended(&self) {
        self.ended
            .send_modify(|generation| *generation = generation.wrapping_add(1));
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

    #[tokio::test]
    async fn other_sessions_end_and_their_waiters_wake() {
        let sessions = Sessions::default();
        let kept = sessions.start().expect("session starts").0;
        let other = sessions.start().expect("session starts").0;
        sessions.start().expect("session starts");

        let waiting = sessions.until_ended(&other);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        assert_eq!(sessions.end_others(&kept), 2);
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("the waiter wakes once its session ends");
        assert!(sessions.is_active(&kept));
        assert!(!sessions.is_active(&other));
    }

    #[test]
    fn session_cookie_is_locked_down() {
        let cookie = SessionToken("abc".to_owned()).into_cookie().to_string();
        for attribute in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
            assert!(cookie.contains(attribute), "{cookie}");
        }
    }
}
