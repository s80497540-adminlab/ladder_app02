use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rand::rngs::OsRng;
use rand::RngCore;

#[derive(Clone, Debug)]
pub enum SignerError {
    WalletNotConnected,
    AutoSignDisabled,
    NoActiveSession,
    SessionExpired,
    InvalidRequest(String),
}

#[derive(Clone, Debug)]
pub struct SessionId(pub String);

#[derive(Clone, Debug)]
pub struct Signature(pub String);

#[derive(Clone, Debug)]
pub struct SignRequest {
    pub ticker: String,
    pub side: String,
    pub size: f64,
    pub leverage: f64,
    pub ts_unix: u64,
}

#[derive(Clone, Debug)]
pub struct SessionState {
    pub active: bool,
    pub session_id: Option<SessionId>,
    pub created_at_unix: Option<u64>,
    pub expires_at_unix: Option<u64>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            active: false,
            session_id: None,
            created_at_unix: None,
            expires_at_unix: None,
        }
    }
}

/// This manager is intentionally “dumb but strict”:
/// - It does NOT store keys.
/// - It enforces: wallet_connected + auto_sign + active session.
/// - It generates an in-memory session id.
/// - It can produce “mock signatures” for now (so you can wire the pipeline).
pub struct SignerManager {
    wallet_connected: bool,
    auto_sign_enabled: bool,
    session: SessionState,
}

impl Default for SignerManager {
    fn default() -> Self {
        Self {
            wallet_connected: false,
            auto_sign_enabled: false,
            session: SessionState::default(),
        }
    }
}

impl SignerManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_wallet_connected(&mut self, connected: bool, now_unix: u64) {
        self.wallet_connected = connected;

        // If wallet disconnects, session must die.
        if !connected {
            self.auto_sign_enabled = false;
            self.revoke_session();
        } else {
            // keep auto_sign as-is (caller typically toggles it)
            self.tick(now_unix);
        }
    }

    pub fn set_auto_sign_enabled(&mut self, enabled: bool, now_unix: u64) -> Result<(), SignerError> {
        if enabled && !self.wallet_connected {
            return Err(SignerError::WalletNotConnected);
        }

        self.auto_sign_enabled = enabled;

        // Turning it off kills the session.
        if !enabled {
            self.revoke_session();
        } else {
            self.tick(now_unix);
        }

        Ok(())
    }

    pub fn session_state(&self) -> SessionState {
        self.session.clone()
    }

    pub fn create_session(&mut self, now_unix: u64, ttl_minutes: u64) -> Result<SessionId, SignerError> {
        if !self.wallet_connected {
            return Err(SignerError::WalletNotConnected);
        }
        if !self.auto_sign_enabled {
            return Err(SignerError::AutoSignDisabled);
        }

        let ttl = ttl_minutes.max(1).min(24 * 60);
        let expires = now_unix.saturating_add(ttl * 60);

        let id = SessionId(generate_session_id());

        self.session.active = true;
        self.session.session_id = Some(id.clone());
        self.session.created_at_unix = Some(now_unix);
        self.session.expires_at_unix = Some(expires);

        Ok(id)
    }

    pub fn revoke_session(&mut self) {
        self.session = SessionState::default();
    }

    pub fn tick(&mut self, now_unix: u64) -> bool {
        // returns true if session changed
        if !self.session.active {
            return false;
        }
        if let Some(exp) = self.session.expires_at_unix {
            if now_unix >= exp {
                self.revoke_session();
                return true;
            }
        }
        false
    }

    pub fn can_sign(&self, now_unix: u64) -> Result<(), SignerError> {
        if !self.wallet_connected {
            return Err(SignerError::WalletNotConnected);
        }
        if !self.auto_sign_enabled {
            return Err(SignerError::AutoSignDisabled);
        }
        if !self.session.active {
            return Err(SignerError::NoActiveSession);
        }
        if let Some(exp) = self.session.expires_at_unix {
            if now_unix >= exp {
                return Err(SignerError::SessionExpired);
            }
        }
        Ok(())
    }

    /// “Mock-sign” a request so you can wire the pipeline end-to-end.
    /// Later we replace this with real wallet signing.
    pub fn sign_request(&self, req: &SignRequest, now_unix: u64) -> Result<Signature, SignerError> {
        self.can_sign(now_unix)?;

        if !req.size.is_finite() || req.size <= 0.0 {
            return Err(SignerError::InvalidRequest("size must be > 0".to_string()));
        }
        if !req.leverage.is_finite() || req.leverage <= 0.0 {
            return Err(SignerError::InvalidRequest("leverage must be > 0".to_string()));
        }
        if req.ticker.trim().is_empty() {
            return Err(SignerError::InvalidRequest("ticker empty".to_string()));
        }
        if req.side.trim().is_empty() {
            return Err(SignerError::InvalidRequest("side empty".to_string()));
        }

        // Deterministic "signature" based on request + session id.
        let mut h = DefaultHasher::new();
        req.ticker.hash(&mut h);
        req.side.hash(&mut h);
        (req.size.to_bits()).hash(&mut h);
        (req.leverage.to_bits()).hash(&mut h);
        req.ts_unix.hash(&mut h);

        if let Some(SessionId(id)) = &self.session.session_id {
            id.hash(&mut h);
        }

        let sig_u64 = h.finish();
        Ok(Signature(format!("mock_sig_{:016x}", sig_u64)))
    }
}

fn generate_session_id() -> String {
    let mut buf = [0u8; 16];
    OsRng.fill_bytes(&mut buf);
    hex16(&buf)
}

fn hex16(bytes: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
