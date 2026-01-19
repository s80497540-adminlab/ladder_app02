# Order Types Support

This application now supports all dYdX order types through the underlying protocol.

## Supported Order Types

### 1. Market Orders
**Type:** `"market"`
- Executes immediately at the best available price
- Includes 0.5% slippage protection (buys at 1.005x, sells at 0.995x)
- Default short-term order (10 blocks ~36 seconds)
- **Required fields:** ticker, side, size
- **Optional fields:** reduce_only

### 2. Limit Orders
**Type:** `"limit"`
- Executes only at the specified price or better
- Requires valid `price_hint` (the limit price)
- Can be short-term (IOC/FOK) or long-term (GTC)
- **Required fields:** ticker, side, size, price_hint
- **Optional fields:** reduce_only, post_only, time_in_force

### 3. Stop-Limit Orders
**Type:** `"stop_limit"`
- Conditional order that becomes a limit order when trigger price is hit
- Long-term order type (stored on-chain)
- **Required fields:** ticker, side, size, price_hint (limit price), trigger_price
- **Optional fields:** reduce_only, time_in_force

### 4. Stop-Market Orders
**Type:** `"stop_market"`
- Conditional order that becomes a market order when trigger price is hit
- Converts to IOC market order for immediate execution
- Long-term order type (stored on-chain)
- **Required fields:** ticker, side, size, trigger_price
- **Optional fields:** reduce_only

### 5. Take-Profit Limit Orders
**Type:** `"take_profit_limit"`
- Conditional order that becomes a limit order when target price is reached
- Long-term order type (stored on-chain)
- **Required fields:** ticker, side, size, price_hint (limit price), trigger_price
- **Optional fields:** reduce_only, time_in_force

### 6. Take-Profit Market Orders
**Type:** `"take_profit_market"`
- Conditional order that becomes a market order when target price is reached
- Converts to IOC market order for immediate execution
- Long-term order type (stored on-chain)
- **Required fields:** ticker, side, size, trigger_price
- **Optional fields:** reduce_only

## Time In Force Options

### For Limit Orders:
- **GTC** (Good-Til-Cancel / `"gtc"`): Long-term order, stays on book until filled or cancelled (~1 hour expiration)
- **IOC** (Immediate-Or-Cancel / `"ioc"`): Executes immediately, unfilled portion cancelled
- **FOK** (Fill-Or-Kill / `"fok"`): Must fill completely or cancelled entirely
- **POST_ONLY** (`"post_only"`): Only adds liquidity, never takes (maker-only)

### For Conditional Orders (Stop/Take-Profit):
- **DEFAULT**: Standard execution when triggered
- **POST_ONLY**: Maker-only execution when triggered
- **IOC**: Immediate execution when triggered (for market variants)
- **FOK**: Fill completely or cancel when triggered

## Order Flags

### post_only (boolean)
- When `true`, order will only execute as a maker (adds liquidity)
- Prevents order from crossing the spread and paying taker fees
- Useful for limit orders to ensure maker rebates

### reduce_only (boolean)
- When `true`, order can only reduce existing position size
- Cannot open new positions or increase current position
- Safety mechanism for closing positions

## Current UI Integration

As of now, the UI only exposes **Market Orders** by default. The order type infrastructure is fully implemented in the backend, but the UI needs to be updated to:

1. Add order type selector (dropdown with all 6 types)
2. Add limit price input field (for limit and stop-limit orders)
3. Add trigger price input field (for stop and take-profit orders)
4. Add time-in-force selector (for limit orders)
5. Add post_only checkbox (for limit orders)

## Code Example

```rust
// Market order (currently used)
trade_engine::OrderRequest {
    ticker: "BTC-USD".to_string(),
    side: "Buy".to_string(),
    order_type: "market".to_string(),
    size: 0.01,
    leverage: 1.0,
    price_hint: 50000.0,  // Used for slippage protection
    trigger_price: None,
    post_only: false,
    time_in_force: None,
    master_address: "dydx1...".to_string(),
    session_mnemonic: "...".to_string(),
    authenticator_id: 1,
    grpc_endpoint: "https://...".to_string(),
    chain_id: "dydx-mainnet-1".to_string(),
    reduce_only: false,
}

// Limit order example
trade_engine::OrderRequest {
    // ... same fields as above
    order_type: "limit".to_string(),
    price_hint: 49000.0,  // Limit price
    post_only: true,  // Maker-only
    time_in_force: Some("gtc".to_string()),
    // ...
}

// Stop-loss example
trade_engine::OrderRequest {
    // ... same fields as above
    order_type: "stop_market".to_string(),
    trigger_price: Some(48000.0),  // Stop price
    // ...
}
```

## Implementation Details

The order type handling is implemented in `src/trade_engine.rs`:
- `OrderRequest` struct contains all necessary fields
- `place_order()` function uses pattern matching to build the appropriate order
- Short-term orders expire in ~10 blocks (36 seconds)
- Long-term orders (conditional and GTC limits) expire in ~1000 blocks (1 hour)

## Next Steps for UI Integration

To fully expose these order types in the UI:

1. Extend the Slint UI definition with new input fields
2. Add state management for order type, limit price, and trigger price
3. Update the order submission logic to pass user-selected values
4. Add validation for required fields based on order type
5. Display appropriate help text explaining each order type
