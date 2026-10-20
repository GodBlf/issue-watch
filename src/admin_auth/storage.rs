use super::{Auth, LIFETIME};
use anyhow::{ensure, Context, Result};
use argon2::{
    password_hash::{PasswordHash, SaltString},
    Argon2, PasswordHasher, PasswordVerifier,
};
use rand_core::{OsRng, RngCore};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use sha2::{Digest, Sha256};
fn digest(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
impl Auth {
    pub(super) fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(self.path.as_ref())?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }
    /// Atomically changes the password and revokes all sessions, including in running servers.
    /// Hashing is synchronous; async callers must use a blocking worker.
    pub fn reset_password(&self, password: &str) -> Result<()> {
        ensure!(
            (16..=1024).contains(&password.len()),
            "password must contain 16 to 1024 UTF-8 bytes"
        );
        let salt = SaltString::generate(&mut OsRng);
        let hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|_| anyhow::anyhow!("password hashing failed"))?
            .to_string();
        let mut connection = self.connection()?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO admin_credentials(id,password_hash) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET password_hash=excluded.password_hash",[&hash])?;
        tx.execute("DELETE FROM admin_sessions", [])?;
        tx.commit()?;
        Ok(())
    }
    pub(super) fn login(&self, password: &str) -> Result<Option<String>> {
        let mut connection = self.connection()?;
        let original: Option<String> = connection
            .query_row(
                "SELECT password_hash FROM admin_credentials WHERE id=1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(original) = original else {
            return Ok(None);
        };
        let hash = PasswordHash::new(&original)
            .map_err(|_| anyhow::anyhow!("invalid stored password hash"))?;
        if Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_err()
        {
            return Ok(None);
        }
        let mut bytes = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut bytes)
            .context("session randomness unavailable")?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let now = (self.clock)();
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Recheck under the write lock: reset cannot race old-password verification
        // into issuing a session after the reset has committed.
        let current: String = tx.query_row(
            "SELECT password_hash FROM admin_credentials WHERE id=1",
            [],
            |row| row.get(0),
        )?;
        if current != original {
            return Ok(None);
        }
        tx.execute("DELETE FROM admin_sessions WHERE expires_at<=?1", [now])?;
        tx.execute(
            "INSERT INTO admin_sessions(digest,created_at,expires_at) VALUES(?1,?2,?3)",
            params![digest(&token), now, now + LIFETIME],
        )?;
        tx.commit()?;
        Ok(Some(token))
    }
    pub(super) fn valid_session(&self, token: &str) -> Result<bool> {
        Ok(self.connection()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_sessions WHERE digest=?1 AND expires_at>?2)",
            params![digest(token), (self.clock)()],
            |row| row.get(0),
        )?)
    }
    pub(super) fn revoke(&self, token: &str) -> Result<()> {
        self.connection()?.execute(
            "DELETE FROM admin_sessions WHERE digest=?1",
            [digest(token)],
        )?;
        Ok(())
    }
}
