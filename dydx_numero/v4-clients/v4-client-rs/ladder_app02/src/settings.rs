use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl Network {
    pub fn as_str(&self) -> &'static str {
        match self {
            Network::Mainnet => "Mainnet",
            Network::Testnet => "Testnet",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "mainnet" => Network::Mainnet,
            _ => Network::Testnet,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettingsState {
    // UI-facing fields
    pub wallet_address: String,
    pub wallet_status: String,

    pub network: Network,
    pub rpc_endpoint: String,

    pub auto_sign: bool,
    pub session_ttl_minutes: u64,
    pub signer_status: String,

    pub last_error: String,

    // Internal bits (no secrets stored)
    pub wallet_connected: bool,
    pub session_active: bool,
    pub session_expires_at_unix: Option<u64>,
}

impl Default for SettingsState {
    fn default() -> Self {
        Self {
            wallet_address: String::new(),
            wallet_status: "disconnected".to_string(),

            network: Network::Testnet,
            rpc_endpoint: String::new(),

            auto_sign: false,
            session_ttl_minutes: 30,
            signer_status: "inactive".to_string(),

            last_error: String::new(),

            wallet_connected: false,
            session_active: false,
            session_expires_at_unix: None,
        }
    }
}

pub struct SettingsManager {
    base_dir: PathBuf,
    cfg_path: PathBuf,
    state: SettingsState,
}

impl SettingsManager {
    pub fn new(base_dir: PathBuf) -> Self {
        let cfg_path = base_dir.join("settings.conf");
        let mut mgr = Self {
            base_dir,
            cfg_path,
            state: SettingsState::default(),
        };
        mgr.load_from_disk();
        mgr.recompute_statuses(0);
        mgr
    }

    pub fn state(&self) -> SettingsState {
        self.state.clone()
    }

    pub fn tick(&mut self, now_unix: u64) -> bool {
        // Returns true if something changed (eg. session expired)
        let mut changed = false;

        if self.state.session_active {
            if let Some(exp) = self.state.session_expires_at_unix {
                if now_unix >= exp {
                    self.state.session_active = false;
                    self.state.session_expires_at_unix = None;
                    changed = true;
                }
            }
        }

        if changed {
            self.recompute_statuses(now_unix);
            // no need to save; session is ephemeral by design
        }

        changed
    }

    pub fn connect_wallet(&mut self, now_unix: u64, wallet_address: String) {
        self.state.last_error.clear();

        let addr = wallet_address.trim().to_string();
        if addr.is_empty() {
            self.state.last_error = "Wallet address is empty.".to_string();
            self.recompute_statuses(now_unix);
            return;
        }

        // IMPORTANT: We are NOT importing private keys here. This is just the
        // connection “state machine” + UI wiring. Real wallet integration comes later.
        self.state.wallet_address = addr;
        self.state.wallet_connected = true;

        self.recompute_statuses(now_unix);
        self.save_to_disk();
    }

    pub fn disconnect_wallet(&mut self, now_unix: u64) {
        self.state.last_error.clear();

        self.state.wallet_connected = false;
        self.state.auto_sign = false;
        self.state.session_active = false;
        self.state.session_expires_at_unix = None;

        self.recompute_statuses(now_unix);
        self.save_to_disk();
    }

    pub fn refresh_status(&mut self, now_unix: u64) {
        self.state.last_error.clear();
        self.recompute_statuses(now_unix);
        // no save needed
    }

    pub fn select_network(&mut self, now_unix: u64, net: Network) {
        self.state.last_error.clear();
        self.state.network = net;
        self.recompute_statuses(now_unix);
        self.save_to_disk();
    }

    pub fn apply_rpc(&mut self, now_unix: u64, endpoint: String) {
        self.state.last_error.clear();
        self.state.rpc_endpoint = endpoint.trim().to_string();
        self.recompute_statuses(now_unix);
        self.save_to_disk();
    }

    pub fn toggle_auto_sign(&mut self, now_unix: u64, enabled: bool) {
        self.state.last_error.clear();

        if enabled && !self.state.wallet_connected {
            self.state.last_error = "Connect wallet first.".to_string();
            self.state.auto_sign = false;
            self.state.session_active = false;
            self.state.session_expires_at_unix = None;
            self.recompute_statuses(now_unix);
            return;
        }

        self.state.auto_sign = enabled;

        if !enabled {
            self.state.session_active = false;
            self.state.session_expires_at_unix = None;
        }

        self.recompute_statuses(now_unix);
        self.save_to_disk();
    }

    pub fn create_session(&mut self, now_unix: u64, ttl_minutes_str: String) {
        self.state.last_error.clear();

        if !self.state.wallet_connected {
            self.state.last_error = "Connect wallet first.".to_string();
            self.recompute_statuses(now_unix);
            return;
        }
        if !self.state.auto_sign {
            self.state.last_error = "Enable Auto-sign first.".to_string();
            self.recompute_statuses(now_unix);
            return;
        }

        let ttl = ttl_minutes_str.trim().parse::<u64>().ok().unwrap_or(30);
        let ttl = ttl.max(1).min(24 * 60);

        self.state.session_ttl_minutes = ttl;
        self.state.session_active = true;
        self.state.session_expires_at_unix = Some(now_unix.saturating_add(ttl * 60));

        self.recompute_statuses(now_unix);
        self.save_to_disk();
    }

    pub fn revoke_session(&mut self, now_unix: u64) {
        self.state.last_error.clear();

        self.state.session_active = false;
        self.state.session_expires_at_unix = None;

        self.recompute_statuses(now_unix);
        // no save required, but harmless if we keep TTL / auto_sign persisted
        self.save_to_disk();
    }

    // -----------------------------
    // Internals: statuses + config
    // -----------------------------

    fn recompute_statuses(&mut self, now_unix: u64) {
        // Wallet status
        self.state.wallet_status = if self.state.wallet_connected {
            let net = self.state.network.as_str();
            let rpc = if self.state.rpc_endpoint.trim().is_empty() {
                "rpc:default".to_string()
            } else {
                "rpc:custom".to_string()
            };
            format!("connected | {net} | {rpc}")
        } else {
            "disconnected".to_string()
        };

        // Signer status
        self.state.signer_status = if !self.state.wallet_connected {
            "inactive".to_string()
        } else if !self.state.auto_sign {
            "ready (no session)".to_string()
        } else if self.state.session_active {
            if let Some(exp) = self.state.session_expires_at_unix {
                let secs_left = exp.saturating_sub(now_unix);
                let mins_left = secs_left / 60;
                format!("session active ({}m left)", mins_left)
            } else {
                "session active".to_string()
            }
        } else {
            "ready (session not created)".to_string()
        };
    }

    fn load_from_disk(&mut self) {
        if !self.cfg_path.exists() {
            return;
        }
        let Ok(f) = File::open(&self.cfg_path) else { return; };
        let reader = BufReader::new(f);

        // very small key=value config (no extra deps)
        for line in reader.lines().flatten() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue; };
            let k = k.trim();
            let v = v.trim();

            match k {
                "wallet_address" => self.state.wallet_address = v.to_string(),
                "network" => self.state.network = Network::from_str(v),
                "rpc_endpoint" => self.state.rpc_endpoint = v.to_string(),
                "auto_sign" => self.state.auto_sign = v.eq_ignore_ascii_case("true") || v == "1",
                "session_ttl_minutes" => {
                    if let Ok(n) = v.parse::<u64>() {
                        self.state.session_ttl_minutes = n.max(1).min(24 * 60);
                    }
                }
                _ => {}
            }
        }

        // NOTE:
        // We intentionally do NOT persist wallet_connected/session_active.
        // Those are runtime/session concepts.
        self.state.wallet_connected = false;
        self.state.session_active = false;
        self.state.session_expires_at_unix = None;
    }

    fn save_to_disk(&self) {
        if let Err(e) = create_dir_all(&self.base_dir) {
            eprintln!("[settings] failed to create base_dir {}: {e}", self.base_dir.display());
            return;
        }

        let tmp = self.base_dir.join("settings.conf.tmp");
        let mut f = match File::create(&tmp) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[settings] failed to write {}: {e}", tmp.display());
                return;
            }
        };

        let _ = writeln!(f, "# ladder_app02 settings (no secrets stored)");
        let _ = writeln!(f, "wallet_address={}", self.state.wallet_address);
        let _ = writeln!(f, "network={}", self.state.network.as_str());
        let _ = writeln!(f, "rpc_endpoint={}", self.state.rpc_endpoint);
        let _ = writeln!(f, "auto_sign={}", if self.state.auto_sign { "true" } else { "false" });
        let _ = writeln!(f, "session_ttl_minutes={}", self.state.session_ttl_minutes);

        // Atomic-ish replace
        let _ = std::fs::rename(tmp, &self.cfg_path);
    }
}
