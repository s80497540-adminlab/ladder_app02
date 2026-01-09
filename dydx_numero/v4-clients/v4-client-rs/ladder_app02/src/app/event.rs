use super::state::MidTick;
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

    SendOrder,
    ReloadData,
    RunScript,
    Deposit { amount: f32 },
    Withdraw { amount: f32 },

    TradeRealModeToggled { enabled: bool },
    ArmRealRequest { phrase: String },
    DisarmReal,
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
}

#[derive(Debug, Clone)]
pub enum TimerEvent {
    Tick1s { now_unix: u64 },
}
