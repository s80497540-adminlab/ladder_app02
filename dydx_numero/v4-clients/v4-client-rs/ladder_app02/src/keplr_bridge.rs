use crate::app::{AppEvent, ExecEvent};
use crate::app::state::{KeplrSessionRecord, KEPLR_SESSION_VERSION};
use anyhow::{anyhow, Context, Result};
use base64::Engine;
use bip32::Mnemonic;
use cosmrs::crypto::PublicKey as CosmosPublicKey;
use cosmrs::tendermint::chain::Id as TendermintId;
use cosmrs::tendermint::public_key::PublicKey as TmPublicKey;
use cosmrs::proto::cosmos::tx::v1beta1::TxRaw as ProtoTxRaw;
use cosmrs::tx::{self, SignDoc, SignerInfo};
use dydx::indexer::{Address, Denom};
use dydx::node::{ChainId, NodeClient, NodeConfig, Wallet};
use dydx_proto::dydxprotocol::accountplus::MsgAddAuthenticator;
use dydx_proto::ToAny;
use rustls::crypto::ring;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tiny_http::{Header, Method, Response, Server};
use tokio::runtime::Runtime;

const DEFAULT_MAINNET_GRPC: &str = "https://dydx-ops-grpc.kingnodes.com:443";
const DEFAULT_TESTNET_GRPC: &str = "https://test-dydx-grpc.kingnodes.com";
const DEFAULT_MAINNET_FEE_DENOM: &str =
    "ibc/8E27BA2D5493AF5636760E354E46004562C46AB7EC0CC4C1CA14E9E20E2545B5";
const AUTHENTICATOR_GAS_USED: u64 = 200_000;

#[derive(Clone, Debug)]
pub struct KeplrBridgeConfig {
    pub chain_id: String,
    pub grpc_endpoint: String,
    pub fee_denom: String,
    pub session_ttl_minutes: u64,
}

#[derive(Debug, Serialize)]
struct AuthJson {
    #[serde(rename = "type")]
    kind: String,
    config: String,
}

#[derive(Clone)]
struct SignPayload {
    body_bytes: Vec<u8>,
    auth_info_bytes: Vec<u8>,
    account_number: u64,
    chain_id: String,
}

#[derive(Default)]
struct BridgeState {
    wallet_address: Option<String>,
    wallet_pubkey: Option<Vec<u8>>,
    payload: Option<SignPayload>,
}

#[derive(Deserialize)]
struct WalletPayload {
    address: String,
    pubkey_base64: String,
}

#[derive(Deserialize)]
struct SignedPayload {
    signature_base64: String,
    body_bytes: String,
    auth_info_bytes: String,
}

#[derive(Serialize)]
struct PayloadResponse {
    body_bytes: String,
    auth_info_bytes: String,
    account_number: u64,
    chain_id: String,
}

pub fn start_keplr_bridge(tx: Sender<AppEvent>, cfg: KeplrBridgeConfig) -> Result<()> {
    let tx_err = tx.clone();
    thread::spawn(move || {
        if let Err(err) = run_bridge(tx, cfg) {
            let _ = tx_err.send(AppEvent::Exec(ExecEvent::KeplrSessionFailed {
                message: format!("Keplr bridge failed: {err}"),
            }));
        }
    });
    Ok(())
}

fn run_bridge(tx: Sender<AppEvent>, cfg: KeplrBridgeConfig) -> Result<()> {
    let _ = ring::default_provider().install_default();
    let rt = Runtime::new().context("init keplr runtime")?;
    let mnemonic = Mnemonic::random(&mut rand::rngs::OsRng, Default::default());
    let session_mnemonic = mnemonic.phrase().to_string();
    let session_wallet = Wallet::from_mnemonic(&session_mnemonic)?;
    let session_account = session_wallet.account_offline(0)?;
    let session_pubkey = session_account.public_key().to_bytes();
    let session_address = session_account.address().to_string();

    let server = Server::http("127.0.0.1:0")
        .map_err(|err| anyhow!("start keplr bridge server: {err}"))?;
    let addr = server.server_addr();
    let url = format!("http://{}", format_addr(addr));
    let _ = tx.send(AppEvent::Exec(ExecEvent::KeplrBridgeReady { url: url.clone() }));

    let state = Arc::new(Mutex::new(BridgeState::default()));
    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let url_path = request.url().to_string();
        match (method, url_path.as_str()) {
            (Method::Get, "/") => {
                let html = bridge_html(&url, &cfg.chain_id);
                let response = Response::from_string(html).with_header(content_type("text/html"));
                let _ = request.respond(response);
            }
            (Method::Get, "/payload") => {
                let payload = {
                    let guard = state.lock().unwrap();
                    guard.payload.clone()
                };
                if let Some(payload) = payload {
                    let body = PayloadResponse {
                        body_bytes: b64().encode(payload.body_bytes),
                        auth_info_bytes: b64().encode(payload.auth_info_bytes),
                        account_number: payload.account_number,
                        chain_id: payload.chain_id,
                    };
                    let response = Response::from_string(serde_json::to_string(&body)?)
                        .with_header(content_type("application/json"));
                    let _ = request.respond(response);
                } else {
                    let response = Response::from_string("{\"error\":\"no payload\"}")
                        .with_header(content_type("application/json"))
                        .with_status_code(400);
                    let _ = request.respond(response);
                }
            }
            (Method::Post, "/wallet") => {
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body)?;
                let payload: WalletPayload = serde_json::from_str(&body)?;
                let pubkey_bytes = b64().decode(payload.pubkey_base64.as_bytes())?;
                {
                    let mut guard = state.lock().unwrap();
                    guard.wallet_address = Some(payload.address.clone());
                    guard.wallet_pubkey = Some(pubkey_bytes.clone());
                }
                let _ = tx.send(AppEvent::Exec(ExecEvent::KeplrWalletConnected {
                    address: payload.address.clone(),
                }));

                let payload = build_sign_payload(
                    &rt,
                    &cfg,
                    &payload.address,
                    &pubkey_bytes,
                    &session_pubkey,
                )?;
                {
                    let mut guard = state.lock().unwrap();
                    guard.payload = Some(payload);
                }
                let response = Response::from_string("{\"ok\":true}")
                    .with_header(content_type("application/json"));
                let _ = request.respond(response);
            }
            (Method::Post, "/submit") => {
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body)?;
                let signed: SignedPayload = serde_json::from_str(&body)?;
                let signature = b64().decode(signed.signature_base64.as_bytes())?;
                let body_bytes = b64().decode(signed.body_bytes.as_bytes())?;
                let auth_info_bytes = b64().decode(signed.auth_info_bytes.as_bytes())?;

                let tx_raw = tx::Raw::from(ProtoTxRaw {
                    body_bytes,
                    auth_info_bytes,
                    signatures: vec![signature],
                });

                let mut client = connect_node(&rt, &cfg)?;
                let tx_hash = rt.block_on(client.broadcast_transaction(tx_raw))?;
                let mut authenticator_id = None;
                for _ in 0..8 {
                    let mut auths = rt.block_on(
                        client
                            .authenticators()
                            .get_authenticators(payload_master_address(&state)?),
                    )?;
                    auths.sort_by_key(|a| a.id);
                    if let Some(last) = auths.last() {
                        authenticator_id = Some(last.id);
                        break;
                    }
                    thread::sleep(Duration::from_secs(1));
                }
                let authenticator_id =
                    authenticator_id.ok_or_else(|| anyhow!("no authenticator id returned"))?;

                let now = now_unix();
                let expires_at = now.saturating_add(cfg.session_ttl_minutes.max(1) * 60);
                let record = KeplrSessionRecord {
                    version: KEPLR_SESSION_VERSION,
                    created_at_unix: now,
                    expires_at_unix: expires_at,
                    network: cfg.chain_id.clone(),
                    rpc_endpoint: cfg.grpc_endpoint.clone(),
                    master_address: payload_master_address(&state)?.to_string(),
                    session_mnemonic: session_mnemonic.clone(),
                    session_address: session_address.clone(),
                    authenticator_id,
                };
                let _ = record.save();

                let _ = tx.send(AppEvent::Exec(ExecEvent::KeplrSessionCreated {
                    session_address,
                    session_mnemonic,
                    master_address: record.master_address,
                    authenticator_id,
                    expires_at_unix: expires_at,
                }));
                let response = Response::from_string(format!(
                    "{{\"ok\":true,\"tx_hash\":\"{}\"}}",
                    tx_hash
                ))
                .with_header(content_type("application/json"));
                let _ = request.respond(response);
                break;
            }
            _ => {
                let response =
                    Response::from_string("{\"error\":\"not found\"}").with_status_code(404);
                let _ = request.respond(response);
            }
        }
    }

    Ok(())
}

fn payload_master_address(state: &Arc<Mutex<BridgeState>>) -> Result<Address> {
    let guard = state.lock().unwrap();
    let addr = guard
        .wallet_address
        .as_ref()
        .ok_or_else(|| anyhow!("missing wallet address"))?;
    addr.parse::<Address>()
        .map_err(|_| anyhow!("invalid wallet address"))
}

fn build_sign_payload(
    rt: &Runtime,
    cfg: &KeplrBridgeConfig,
    wallet_address: &str,
    wallet_pubkey: &[u8],
    session_pubkey: &[u8],
) -> Result<SignPayload> {
    let mut client = connect_node(rt, cfg)?;
    let addr: Address = wallet_address.parse().map_err(|_| anyhow!("invalid address"))?;
    let (account_number, sequence, account_pubkey) = match rt.block_on(client.get_account(&addr)) {
        Ok(account) => {
            let pubkey = account
                .pub_key
                .as_ref()
                .and_then(|any| CosmosPublicKey::try_from(any).ok());
            (account.account_number, account.sequence, pubkey)
        }
        Err(_) => {
            let (account_number, sequence) = rt.block_on(client.query_address(&addr))?;
            (account_number, sequence, None)
        }
    };

    let (auth_type, auth_data) = build_authenticator(session_pubkey)?;
    let msg = MsgAddAuthenticator {
        sender: wallet_address.to_string(),
        authenticator_type: auth_type,
        data: auth_data,
    };

    let fee = build_fee(cfg)?;
    let pubkey = if let Some(pubkey) = account_pubkey {
        pubkey
    } else {
        let tm_key = TmPublicKey::from_raw_secp256k1(wallet_pubkey)
            .ok_or_else(|| anyhow!("invalid Keplr pubkey"))?;
        CosmosPublicKey::try_from(tm_key)
            .map_err(|e| anyhow!("pubkey parse failed: {e}"))?
    };
    let auth_info = SignerInfo::single_direct(Some(pubkey), sequence).auth_info(fee);

    let mut builder = tx::BodyBuilder::new();
    builder.msgs(std::iter::once(msg.to_any())).memo("");
    let body = builder.finish();

    let chain_id = parse_chain_id(&cfg.chain_id)?;
    let tm_chain_id = to_tendermint_id(chain_id)?;
    let sign_doc = SignDoc::new(&body, &auth_info, &tm_chain_id, account_number)
        .map_err(|e| anyhow!("sign doc error: {e}"))?;
    let body_bytes = sign_doc.body_bytes;
    let auth_info_bytes = sign_doc.auth_info_bytes;

    Ok(SignPayload {
        body_bytes,
        auth_info_bytes,
        account_number,
        chain_id: cfg.chain_id.clone(),
    })
}

fn build_authenticator(session_pubkey: &[u8]) -> Result<(String, Vec<u8>)> {
    let auths = vec![
        AuthJson {
            kind: "SignatureVerification".to_string(),
            config: b64().encode(session_pubkey),
        },
        AuthJson {
            kind: "MessageFilter".to_string(),
            config: b64().encode(b"/dydxprotocol.clob.MsgPlaceOrder"),
        },
        AuthJson {
            kind: "SubaccountFilter".to_string(),
            config: b64().encode(b"0"),
        },
    ];
    let json = serde_json::to_string(&auths)?;
    Ok(("AllOf".to_string(), json.into_bytes()))
}

fn build_fee(cfg: &KeplrBridgeConfig) -> Result<tx::Fee> {
    let fee_denom = if cfg.fee_denom.trim().is_empty() {
        DEFAULT_MAINNET_FEE_DENOM.to_string()
    } else {
        cfg.fee_denom.clone()
    };
    let denom = Denom::from_str(&fee_denom)?;
    let chain_id = parse_chain_id(&cfg.chain_id)?;
    let tm_chain_id = to_tendermint_id(chain_id)?;
    let builder = dydx::node::TxBuilder::new(tm_chain_id, denom);
    builder
        .calculate_fee(Some(AUTHENTICATOR_GAS_USED))
        .map_err(|e| anyhow!("fee calc failed: {e}"))
}

fn connect_node(rt: &Runtime, cfg: &KeplrBridgeConfig) -> Result<NodeClient> {
    let chain_id = parse_chain_id(&cfg.chain_id)?;
    let endpoint = if cfg.grpc_endpoint.trim().is_empty() {
        match chain_id {
            ChainId::Mainnet1 => DEFAULT_MAINNET_GRPC.to_string(),
            ChainId::Testnet4 => DEFAULT_TESTNET_GRPC.to_string(),
        }
    } else {
        cfg.grpc_endpoint.clone()
    };
    let fee_denom = if cfg.fee_denom.trim().is_empty() {
        DEFAULT_MAINNET_FEE_DENOM.to_string()
    } else {
        cfg.fee_denom.clone()
    };
    let denom = Denom::from_str(&fee_denom)?;
    let config = NodeConfig {
        endpoint,
        timeout: 2_000,
        chain_id,
        fee_denom: denom,
        manage_sequencing: false,
    };
    rt.block_on(NodeClient::connect(config))
        .map_err(|e| anyhow!("node connect failed: {e}"))
}

fn parse_chain_id(chain_id: &str) -> Result<ChainId> {
    if chain_id.eq_ignore_ascii_case("dydx-mainnet-1") || chain_id.eq_ignore_ascii_case("mainnet")
    {
        Ok(ChainId::Mainnet1)
    } else if chain_id.eq_ignore_ascii_case("dydx-testnet-4")
        || chain_id.eq_ignore_ascii_case("testnet")
    {
        Ok(ChainId::Testnet4)
    } else {
        Err(anyhow!("unsupported chain id: {}", chain_id))
    }
}

fn to_tendermint_id(chain_id: ChainId) -> Result<TendermintId> {
    chain_id
        .try_into()
        .map_err(|_| anyhow!("invalid chain id"))
}

fn content_type(value: &str) -> Header {
    Header::from_bytes(&b"Content-Type"[..], value.as_bytes()).unwrap()
}

fn format_addr(addr: tiny_http::ListenAddr) -> String {
    match addr.to_ip() {
        Some(SocketAddr::V4(v4)) => format!("127.0.0.1:{}", v4.port()),
        Some(SocketAddr::V6(v6)) => format!("[::1]:{}", v6.port()),
        None => "127.0.0.1:0".to_string(),
    }
}

fn b64() -> base64::engine::general_purpose::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

fn bridge_html(base_url: &str, chain_id: &str) -> String {
    format!(
        r#"<!doctype html>
<html>
<head>
  <meta charset="utf-8"/>
  <title>Keplr Session Bridge</title>
  <style>
    body {{ font-family: system-ui, sans-serif; padding: 24px; background: #0f1320; color: #e8f0ff; }}
    .card {{ background: #151a2a; padding: 16px; border-radius: 8px; border: 1px solid #27324a; max-width: 540px; }}
    button {{ background: #2b65ff; color: white; border: none; padding: 10px 14px; border-radius: 6px; cursor: pointer; }}
    code {{ background: #0b0f1a; padding: 2px 6px; border-radius: 4px; }}
  </style>
</head>
<body>
  <div class="card">
    <h2>Keplr Session Bridge</h2>
    <p>This page creates a permissioned session key for fast trading.</p>
    <button id="connect">Connect Keplr & Create Session</button>
    <p id="status"></p>
    <p>Bridge URL: <code>{base_url}</code></p>
  </div>
  <script>
    const apiBase = "{base_url}";
    const statusEl = document.getElementById("status");
    const btn = document.getElementById("connect");
    function status(msg) {{ statusEl.textContent = msg; }}
    function b64ToBytes(b64) {{
      const bin = atob(b64);
      const out = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
      return out;
    }}
    async function connect() {{
      if (!window.keplr) {{
        status("Keplr not detected. Install the extension and refresh.");
        return;
      }}
      status("Connecting to Keplr...");
      const chainId = "{chain_id}";
      await window.keplr.enable(chainId);
      const key = await window.keplr.getKey(chainId);
      const pubkeyBase64 = btoa(String.fromCharCode(...key.pubKey));
      await fetch(apiBase + "/wallet", {{
        method: "POST",
        headers: {{ "Content-Type": "application/json" }},
        body: JSON.stringify({{ address: key.bech32Address, pubkey_base64: pubkeyBase64 }})
      }});
      status("Building sign doc...");
      const payload = await fetch(apiBase + "/payload").then(r => r.json());
      const signDoc = {{
        bodyBytes: b64ToBytes(payload.body_bytes),
        authInfoBytes: b64ToBytes(payload.auth_info_bytes),
        chainId: payload.chain_id,
        accountNumber: String(payload.account_number)
      }};
      status("Requesting signature...");
      const signed = await window.keplr.signDirect(chainId, key.bech32Address, signDoc);
      const signedBody = btoa(String.fromCharCode(...signed.signed.bodyBytes));
      const signedAuth = btoa(String.fromCharCode(...signed.signed.authInfoBytes));
      await fetch(apiBase + "/submit", {{
        method: "POST",
        headers: {{ "Content-Type": "application/json" }},
        body: JSON.stringify({{
          signature_base64: signed.signature.signature,
          body_bytes: signedBody,
          auth_info_bytes: signedAuth
        }})
      }});
      status("Session created. You can close this tab.");
    }}
    btn.addEventListener("click", () => connect().catch(err => status(String(err))));
  </script>
</body>
</html>"#,
        base_url = base_url,
        chain_id = chain_id
    )
}
