#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub ticker: String,
    pub side: String,
    pub size: f64,
    pub leverage: f64,
    pub real: bool,
}

#[derive(Debug, Clone)]
pub enum OrderResult {
    Ack { order_id: String },
    Reject { reason: String },
    Fill { order_id: String, price: f64, size: f64 },
}
