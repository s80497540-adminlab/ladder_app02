use super::state::{MidTick, OpenOrderInfo};
use crate::feed_shared::BookLevel;

#[derive(Debug, Clone)]
pub enum AppEvent {
    Ui(UiEvent),
    Feed(FeedEvent),
    Exec(ExecEvent),
    Timer(TimerEvent),
    HistoryLoaded {
        ticker: String,
        ticks: Vec<MidTick>,
        full: bool,
    },
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    TickerChanged { ticker: String },
    ModeChanged { mode: String },
    TimeModeChanged { time_mode: String },

    FeedEnabledChanged { enabled: bool },
    ChartEnabledChanged { enabled: bool },
    DepthPanelToggled { enabled: bool },
    TradesPanelToggled { enabled: bool },
    VolumePanelToggled { enabled: bool },

    CandleTfChanged { tf_secs: i32 },
    CandleWindowChanged { window_min: i32 },
    CandlePriceModeChanged { mode: String },
    DomDepthChanged { depth: i32 },
    RenderModeChanged { full: bool },
    HistoryValveChanged { open: bool },
    SessionRecordingChanged { enabled: bool },
    ChartViewModeChanged { mode: String },
    HeatmapEnabledChanged { enabled: bool },
    CloseAndSaveRequested,
    DrawToolChanged { tool: String },
    DrawBegin { x: f32, y: f32 },
    DrawUpdate { x: f32, y: f32 },
    DrawEnd { x: f32, y: f32 },
    DrawPolySidesDelta { delta: i32 },
    DrawingSelected { id: u64 },
    DrawingDelete { id: u64 },
    DrawingClearAll,
    MarketPollAdjust { delta: i32 },
    TickerFeedToggled { ticker: String, enabled: bool },
    TickerFavoriteToggled { ticker: String, favorite: bool },
    SettingsConnectWallet,
    SettingsDisconnectWallet,
    SettingsRefreshStatus,
    SettingsSelectNetwork { net: String },
    SettingsApplyRpc { endpoint: String },
    SettingsToggleAutoSign { enabled: bool },
    SettingsCreateSession { ttl_minutes: String },
    SettingsRevokeSession,
    SettingsCopyError,

    SendOrder,
    ReloadData,
    RunScript,
    Deposit { amount: f32 },
    Withdraw { amount: f32 },
    TradeSizeTextChanged { text: String },
    TradeSizeChanged { value: f32 },
    TradeLeverageTextChanged { text: String },
    TradeLeverageChanged { value: f32 },
    TradeMarginTextChanged { text: String },
    TradeMarginChanged { value: f32 },
    TradeMarginLinkToggled { linked: bool },
    TradeLimitPriceChanged { text: String },
    TradeTriggerPriceChanged { text: String },
    TradeOrderTypeChanged { order_type: String },
    TradeTimeInForceChanged { tif: String },
    ClosePositionRequested,
    CancelOpenOrdersRequested,

    TradeRealModeToggled { enabled: bool },
    ArmRealRequest { phrase: String },
    DisarmReal,

    CycleRotateLogs,
    CycleToggleAutoRotate { enabled: bool },
}

#[derive(Debug, Clone)]
pub enum FeedEvent {
    // ✅ ADD ts_unix here
    BookTop {
        ts_unix: u64,
        ticker: String,
        best_bid: f64,
        best_ask: f64,
        best_bid_raw: String,
        best_ask_raw: String,
        bid_liq: f64,
        ask_liq: f64,
    },

    Trade {
        ts_unix: u64,
        ticker: String,
        side: String,
        size: String,
        price: f64,
        price_raw: String,
        source: String,
    },
    MarketPrice {
        ts_unix: u64,
        ticker: String,
        mark_price: f64,
        mark_price_raw: String,
        oracle_price: f64,
        oracle_price_raw: String,
    },
    MarketList {
        markets: Vec<MarketInfo>,
    },
    BookLevels {
        ts_unix: u64,
        ticker: String,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
    },
}

#[derive(Debug, Clone)]
pub struct MarketInfo {
    pub ticker: String,
    pub active: bool,
}

#[derive(Debug, Clone)]
pub enum ExecEvent {
    Receipt {
        ts: String,
        ticker: String,
        side: String,
        kind: String,
        size: String,
        status: String,
        comment: String,
    },
    KeplrBridgeReady { url: String },
    KeplrWalletConnected { address: String },
    KeplrSessionCreated {
        session_address: String,
        session_mnemonic: String,
        master_address: String,
        authenticator_id: u64,
        expires_at_unix: u64,
    },
    KeplrSessionFailed { message: String },
    OrderSent { tx_hash: String },
    OrderFailed { message: String },
    OrderCancelStatus { ok: bool, message: String },
    OpenOrdersSnapshot {
        total: usize,
        ticker: String,
        ticker_count: usize,
        orders: Vec<OpenOrderInfo>,
    },
    OpenOrdersError { message: String },
    AccountSnapshot {
        equity: f64,
        free_collateral: f64,
        margin_enabled: bool,
    },
    AccountSnapshotError { message: String },
    PositionSnapshot {
        ticker: String,
        side: String,
        size: f64,
        entry_price: f64,
        unrealized_pnl: f64,
        status: String,
    },
    PositionSnapshotError { message: String },
}

#[derive(Debug, Clone)]
pub enum TimerEvent {
    Tick1s { now_unix: u64 },
}
