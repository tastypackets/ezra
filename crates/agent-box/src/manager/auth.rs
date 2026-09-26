use std::collections::HashSet;
use std::sync::Mutex;

use argon2::Argon2;
use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use axum_extra::extract::cookie::{Cookie, SameSite};

pub const SESSION_COOKIE: &str = "session";

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())?
        .to_string())
}

pub fn password_matches(password: &str, password_hash: &str) -> bool {
    PasswordHash::new(password_hash).is_ok_and(|parsed_hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
    })
}

/// Logged-in sessions, kept in memory: restarting the manager logs everyone out.
#[derive(Debug, Default)]
pub struct Sessions {
    tokens: Mutex<HashSet<String>>,
}

impl Sessions {
    pub fn start(&self) -> Result<String, getrandom::Error> {
        let mut random_bytes = [0_u8; 32];
        getrandom::fill(&mut random_bytes)?;
        let token: String = random_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.tokens.lock().unwrap().insert(token.clone());
        Ok(token)
    }

    pub fn is_active(&self, token: &str) -> bool {
        self.tokens.lock().unwrap().contains(token)
    }

    pub fn end(&self, token: &str) {
        self.tokens.lock().unwrap().remove(token);
    }
}

pub fn session_cookie(token: String) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_matches_only_its_own_hash() {
        let password_hash = hash_password("correct horse").unwrap();
        assert!(password_hash.starts_with("$argon2id$"));
        assert!(password_matches("correct horse", &password_hash));
        assert!(!password_matches("wrong horse", &password_hash));
        assert!(!password_matches("correct horse", "not a hash"));
    }

    #[test]
    fn sessions_are_random_and_can_end() {
        let sessions = Sessions::default();
        let first_token = sessions.start().unwrap();
        let second_token = sessions.start().unwrap();
        assert_eq!(first_token.len(), 64);
        assert_ne!(first_token, second_token);
        assert!(sessions.is_active(&first_token));

        sessions.end(&first_token);
        assert!(!sessions.is_active(&first_token));
        assert!(sessions.is_active(&second_token));
    }
}
