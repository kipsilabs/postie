//! Port of `internal/webauth`: the single administrator, signing key and revocable sessions for
//! the web server. Tokens are HS256 JWTs with the go-pkgz claims shape, so sessions issued by
//! the Go server validate here and vice versa (same `web_*` tables, same secret row).

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::Rng;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Deserializer, Serialize};

use crate::db::Db;

pub const SESSION_DURATION_SECS: i64 = 24 * 60 * 60;
const ISSUER: &str = "postie";
const AUDIENCE: &str = "postie-api";

/// Error texts match the Go errors.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("registration is disabled")]
    RegistrationClosed,
    #[error("invalid credentials")]
    Credentials,
    #[error("username must have 3–128 characters and password 12–72 bytes")]
    InvalidInput,
    #[error("{0}")]
    Other(String),
}

fn other(ctx: &str, e: impl std::fmt::Display) -> AuthError {
    AuthError::Other(format!("{ctx}: {e}"))
}

/// The go-pkgz `token.User` part that Postie fills.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct User {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub picture: String,
}

fn string_or_seq<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum One {
        S(String),
        V(Vec<String>),
    }
    Ok(match Option::<One>::deserialize(d)? {
        Some(One::S(s)) => vec![s],
        Some(One::V(v)) => v,
        None => vec![],
    })
}

/// Go's `jwt.ClaimStrings` marshals a single audience as a plain string; mirror that.
fn single_as_string<S: serde::Serializer>(v: &[String], s: S) -> Result<S::Ok, S::Error> {
    match v {
        [one] => s.serialize_str(one),
        many => many.serialize(s),
    }
}

/// JWT claims (go-pkgz `token.Claims` with registered claims).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claims {
    #[serde(default)]
    pub iss: String,
    #[serde(default)]
    pub sub: String,
    #[serde(
        default,
        deserialize_with = "string_or_seq",
        serialize_with = "single_as_string"
    )]
    pub aud: Vec<String>,
    pub exp: i64,
    #[serde(default)]
    pub iat: i64,
    #[serde(default)]
    pub jti: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
}

pub struct Service {
    db: Db,
    secret: String,
}

fn random_secret() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Service {
    fn enc_key(&self) -> Result<EncodingKey, AuthError> {
        EncodingKey::from_secret(self.secret.as_bytes()).map_err(|e| other("sign session", e))
    }

    fn dec_key(&self) -> Result<DecodingKey, AuthError> {
        DecodingKey::from_secret(self.secret.as_bytes()).map_err(|_| AuthError::Credentials)
    }

    /// Loads (or creates) the persistent signing secret. Concurrent initializers converge on the
    /// same row; a restart does not invalidate valid sessions.
    pub fn new(db: Db) -> Result<Self, AuthError> {
        let secret = {
            let c = db.lock();
            c.execute(
                "INSERT OR IGNORE INTO web_auth_secret(id,secret) VALUES(1,?1)",
                params![random_secret()],
            )
            .map_err(|e| other("initialize signing secret", e))?;
            c.query_row("SELECT secret FROM web_auth_secret WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .map_err(|e| other("read signing secret", e))?
        };
        Ok(Self { db, secret })
    }

    pub fn registration_open(&self) -> Result<bool, AuthError> {
        let n: i64 = self
            .db
            .lock()
            .query_row("SELECT count(*) FROM web_admin", [], |r| r.get(0))
            .map_err(|e| other("read admin", e))?;
        Ok(n == 0)
    }

    pub fn register(&self, username: &str, email: &str, password: &str) -> Result<(), AuthError> {
        let (username, email) = (username.trim(), email.trim());
        // Go measures bytes (`len`), not characters.
        if username.len() < 3
            || username.len() > 128
            || email.len() > 254
            || password.len() < 12
            || password.len() > 72
        {
            return Err(AuthError::InvalidInput);
        }
        if !self.registration_open()? {
            return Err(AuthError::RegistrationClosed);
        }
        let hash =
            bcrypt::hash(password, bcrypt::DEFAULT_COST).map_err(|e| other("hash password", e))?;
        let n = self
            .db
            .lock()
            .execute(
                "INSERT OR IGNORE INTO web_admin(id,username,email,password_hash) VALUES(1,?1,?2,?3)",
                params![username, email, hash],
            )
            .map_err(|e| other("create admin", e))?;
        if n != 1 {
            return Err(AuthError::RegistrationClosed);
        }
        Ok(())
    }

    pub fn login(&self, username: &str, password: &str) -> Result<String, AuthError> {
        let row = self
            .db
            .lock()
            .query_row(
                "SELECT username,email,password_hash FROM web_admin WHERE id=1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| other("read admin", e))?;
        let Some((stored, email, hash)) = row else {
            return Err(AuthError::Credentials);
        };
        // Always verify the hash, including unknown users, to avoid an account oracle.
        let hash_ok = bcrypt::verify(password, &hash).unwrap_or(false);
        if !hash_ok || (username != stored && (email.is_empty() || username != email)) {
            return Err(AuthError::Credentials);
        }
        self.issue(&stored)
    }

    pub fn issue(&self, username: &str) -> Result<String, AuthError> {
        let id = random_secret();
        let now = now_unix();
        let expiry = now + SESSION_DURATION_SECS;
        let claims = Claims {
            iss: ISSUER.into(),
            sub: username.into(),
            aud: vec![AUDIENCE.into()],
            exp: expiry,
            iat: now,
            jti: id.clone(),
            user: Some(User {
                name: username.into(),
                id: username.into(),
                picture: String::new(),
            }),
        };
        let signed =
            jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.enc_key()?)
                .map_err(|e| other("sign session", e))?;
        let c = self.db.lock();
        c.execute(
            "DELETE FROM web_sessions WHERE expires_at<=?1",
            params![now],
        )
        .map_err(|e| other("clean expired sessions", e))?;
        c.execute(
            "INSERT INTO web_sessions(id,expires_at) VALUES(?1,?2)",
            params![id, expiry],
        )
        .map_err(|e| other("create session", e))?;
        Ok(signed)
    }

    /// Pins HS256, issuer, audience, required expiry and not-before, and checks that the session
    /// exists, has not expired and belongs to the administrator.
    pub fn validate(&self, signed: &str) -> Result<Claims, AuthError> {
        let mut v = Validation::new(Algorithm::HS256);
        v.leeway = 0;
        v.validate_nbf = true;
        v.set_issuer(&[ISSUER]);
        v.set_audience(&[AUDIENCE]);
        v.set_required_spec_claims(&["exp", "iss", "aud"]);
        let claims = jsonwebtoken::decode::<Claims>(signed, &self.dec_key()?, &v)
            .map_err(|_| AuthError::Credentials)?
            .claims;
        let username: Option<String> = self
            .db
            .lock()
            .query_row(
                "SELECT a.username FROM web_sessions s CROSS JOIN web_admin a WHERE s.id=?1 AND s.expires_at>?2 AND a.id=1",
                params![claims.jti, now_unix()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| AuthError::Credentials)?;
        match (username, &claims.user) {
            (Some(u), Some(user)) if u == claims.sub && user.id == u => Ok(claims),
            _ => Err(AuthError::Credentials),
        }
    }

    pub fn revoke(&self, id: &str) -> Result<(), AuthError> {
        self.db
            .lock()
            .execute("DELETE FROM web_sessions WHERE id=?1", params![id])
            .map_err(|e| other("revoke session", e))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// DDL from `internal/database/migrations/010_add_web_auth.sql`.
    const DDL: &str =
        "create table web_admin (id integer primary key check (id = 1), username text not null,
        email text not null default '', password_hash text not null);
        create table web_auth_secret (id integer primary key check (id = 1), secret text not null);
        create table web_sessions (id text primary key, expires_at integer not null);";
    const PW: &str = "a-long-secret-password";

    fn service() -> Service {
        let db = Db::in_memory().unwrap();
        db.lock().execute_batch(DDL).unwrap();
        Service::new(db).unwrap()
    }

    #[test]
    fn bootstrap_login_revocation_and_restart() {
        let s = service();
        assert!(s.registration_open().unwrap());
        s.register("admin", "admin@example.com", PW).unwrap();
        assert!(matches!(
            s.register("other", "", "another-long-password"),
            Err(AuthError::RegistrationClosed)
        ));
        assert!(matches!(
            s.login("admin", "incorrect"),
            Err(AuthError::Credentials)
        ));
        let jwt = s.login("admin@example.com", PW).unwrap();
        let claims = s.validate(&jwt).unwrap();
        assert_eq!(claims.sub, "admin");
        let restarted = Service::new(s.db.clone()).unwrap();
        restarted.validate(&jwt).unwrap();
        s.revoke(&claims.jti).unwrap();
        assert!(restarted.validate(&jwt).is_err());
        assert!(s.validate(&format!("{jwt}tampered")).is_err());
    }

    #[test]
    fn registration_is_atomic_across_threads() {
        let s = Arc::new(service());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let s = s.clone();
                std::thread::spawn(move || s.register("admin", "", PW))
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|e| matches!(e, AuthError::RegistrationClosed))
        );
    }

    #[test]
    fn input_limits_count_bytes_like_go() {
        let s = service();
        assert!(matches!(
            s.register("ab", "", PW),
            Err(AuthError::InvalidInput)
        ));
        assert!(matches!(
            s.register("admin", "", "short"),
            Err(AuthError::InvalidInput)
        ));
        assert!(matches!(
            s.register("admin", "", &"x".repeat(73)),
            Err(AuthError::InvalidInput)
        ));
        // 3 chars but 6 bytes passes; 2 chars of 3 bytes each (6 bytes) also passes: bytes, not chars
        s.register("日本", "", PW).unwrap();
    }

    fn resign(c: &Claims, alg: Algorithm, key: &str) -> String {
        jsonwebtoken::encode(
            &Header::new(alg),
            c,
            &EncodingKey::from_secret(key.as_bytes()).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn validate_rejects_invalid_signed_claims() {
        let s = service();
        s.register("admin", "", PW).unwrap();
        let signed = s.login("admin", PW).unwrap();
        let base = s.validate(&signed).unwrap();
        let now = now_unix();
        let cases: Vec<(&str, Box<dyn Fn(&mut Claims)>)> = vec![
            ("issuer", Box::new(|c| c.iss = "altmount".into())),
            (
                "audience",
                Box::new(|c| c.aud = vec!["altmount-api".into()]),
            ),
            ("missing audience", Box::new(|c| c.aud = vec![])),
            ("expired", Box::new(move |c| c.exp = now - 60)),
            ("subject", Box::new(|c| c.sub = "other".into())),
            ("missing user", Box::new(|c| c.user = None)),
            (
                "user identity",
                Box::new(|c| c.user.as_mut().unwrap().id = "other".into()),
            ),
            (
                "unknown session",
                Box::new(|c| c.jti = "unregistered-session".into()),
            ),
        ];
        for (name, modify) in cases {
            let mut c = base.clone();
            modify(&mut c);
            let changed = resign(&c, Algorithm::HS256, &s.secret);
            assert!(
                matches!(s.validate(&changed), Err(AuthError::Credentials)),
                "{name}"
            );
        }
        assert!(matches!(
            s.validate(&resign(&base, Algorithm::HS384, &s.secret)),
            Err(AuthError::Credentials)
        ));
        assert!(matches!(
            s.validate(&resign(&base, Algorithm::HS256, "different-secret")),
            Err(AuthError::Credentials)
        ));
    }

    /// Fixture produced by the Go service (`internal/webauth`): bcrypt hash, signing secret and a
    /// far-future (2100) session token, so sessions and passwords carry over between servers.
    #[test]
    fn go_issued_credentials_and_token_validate() {
        let db = Db::in_memory().unwrap();
        db.lock().execute_batch(DDL).unwrap();
        db.lock().execute("insert into web_admin(id,username,email,password_hash) values(1,'admin','admin@example.com','$2a$10$hUIs9BxLLHvBfbWTX2NH5.Qq4LXAeAPy57fzCj/H/E2LKLvf3Opt.')", []).unwrap();
        db.lock().execute("insert into web_auth_secret(id,secret) values(1,'6a0sihQHlLDfF9ZFhcIQ8XyjUyu-EZc1KLuaZR2D_4M')", []).unwrap();
        db.lock()
            .execute(
                "insert into web_sessions(id,expires_at) values('go-session-id',4102444800)",
                [],
            )
            .unwrap();
        let s = Service::new(db).unwrap();
        let token = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJwb3N0aWUiLCJzdWIiOiJhZG1pbiIsImF1ZCI6InBvc3RpZS1hcGkiLCJleHAiOjQxMDI0NDQ4MDAsImlhdCI6MTc5NTA3OTE5OSwianRpIjoiZ28tc2Vzc2lvbi1pZCIsInVzZXIiOnsibmFtZSI6ImFkbWluIiwiaWQiOiJhZG1pbiIsInBpY3R1cmUiOiIifX0.0_uUKowBvUXa-lJU4PLc0lxY_mibcwrRrndKTj8oqfY";
        let c = s.validate(token).unwrap();
        assert_eq!(
            (c.sub.as_str(), c.jti.as_str(), c.exp),
            ("admin", "go-session-id", 4_102_444_800)
        );
        // the Go bcrypt hash verifies and a login works with the e-mail identity
        assert!(s.login("admin@example.com", PW).is_ok());
        assert!(matches!(
            s.login("admin", "wrong-password-x"),
            Err(AuthError::Credentials)
        ));
    }

    /// Prints a far-future Rust-issued token for the one-off Go validation check (see ADR notes).
    #[test]
    #[ignore = "prints a token for a manual cross-check against the Go service"]
    fn print_rust_token_for_go() {
        let s = service();
        let c = Claims {
            iss: "postie".into(),
            sub: "admin".into(),
            aud: vec!["postie-api".into()],
            exp: 4_102_444_800,
            iat: now_unix(),
            jti: "rust-session-id".into(),
            user: Some(User {
                name: "admin".into(),
                id: "admin".into(),
                picture: String::new(),
            }),
        };
        println!("RUST secret={}", s.secret);
        println!("RUST token={}", resign(&c, Algorithm::HS256, &s.secret));
    }

    #[test]
    fn single_audience_serializes_as_string_like_go() {
        let s = service();
        s.register("admin", "", PW).unwrap();
        let tok = s.login("admin", PW).unwrap();
        let payload = tok.split('.').nth(1).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap();
        assert_eq!(json["aud"], "postie-api");
        assert_eq!(json["user"]["picture"], "");
    }

    #[test]
    fn not_yet_valid_and_missing_expiry_are_rejected() {
        let s = service();
        s.register("admin", "", PW).unwrap();
        let base = s.validate(&s.login("admin", PW).unwrap()).unwrap();
        let mut v = serde_json::to_value(&base).unwrap();
        v["nbf"] = serde_json::json!(now_unix() + 3600);
        let t = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &v,
            &EncodingKey::from_secret(s.secret.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(s.validate(&t).is_err());
        let mut v = serde_json::to_value(&base).unwrap();
        v.as_object_mut().unwrap().remove("exp");
        let t = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &v,
            &EncodingKey::from_secret(s.secret.as_bytes()).unwrap(),
        )
        .unwrap();
        assert!(s.validate(&t).is_err());
    }
}
