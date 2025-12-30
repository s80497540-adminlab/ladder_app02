use super::types::*;

#[derive(Debug, Default)]
pub struct ExecEngine;

impl ExecEngine {
    pub fn new() -> Self {
        Self
    }

    pub fn validate(&self, intent: &OrderIntent) -> Result<(), String> {
        if intent.size <= 0.0 || !intent.size.is_finite() {
            return Err("size must be > 0".to_string());
        }
        if intent.leverage <= 0.0 || !intent.leverage.is_finite() {
            return Err("leverage must be > 0".to_string());
        }
        Ok(())
    }

    // Phase-2 scaffold: stub “place order”
    pub fn place_order(&self, intent: OrderIntent) -> OrderResult {
        if let Err(reason) = self.validate(&intent) {
            return OrderResult::Reject { reason };
        }
        OrderResult::Ack { order_id: "stub-order-id".to_string() }
    }
}
