use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Order side (Buy or Sell)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// Order types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,    // Execute at current market price
    Limit,     // Execute at specified price or better
    StopLoss,  // Market order when price reaches stop price
    StopLimit, // Limit order when price reaches stop price
}

/// Time-in-force enforcement types
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    /// Immediate or Cancel: execute immediately (partially or completely) and cancel any unfilled portion
    IOC,

    /// Fill or Kill: execute immediately and completely or cancel the entire order
    FOK,

    /// Good Till Canceled: order remains active until it is explicitly canceled
    GTC,

    /// Good Till Date: order remains active until the specified date
    GTD(DateTime<Utc>),

    /// Day order: automatically canceled at the end of the trading day
    DAY,
}

/// Order status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    Expired,
}

/// Full order details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: Uuid,
    pub symbol: String,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: u64,
    pub filled_quantity: u64,
    pub price: Option<f64>,      // Required for Limit and StopLimit orders
    pub stop_price: Option<f64>, // Required for StopLoss and StopLimit orders
    pub time_in_force: TimeInForce,
    pub status: OrderStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Order {
    pub fn new(
        symbol: String,
        side: Side,
        order_type: OrderType,
        quantity: u64,
        price: Option<f64>,
        stop_price: Option<f64>,
        time_in_force: TimeInForce,
    ) -> Self {
        let now = Utc::now();

        Self {
            id: Uuid::new_v4(),
            symbol,
            side,
            order_type,
            quantity,
            filled_quantity: 0,
            price,
            stop_price,
            time_in_force,
            status: OrderStatus::New,
            created_at: now,
            updated_at: now,
        }
    }

    /// Validate the order based on order type requirements
    pub fn validate(&self) -> bool {
        match self.order_type {
            OrderType::Market => true, // Market orders don't require price
            OrderType::Limit => self.price.is_some(), // Limit orders require price
            OrderType::StopLoss => self.stop_price.is_some(), // Stop orders require stop price
            OrderType::StopLimit => self.price.is_some() && self.stop_price.is_some(), // Stop-limit requires both
        }
    }

    /// Determine if an order is marketable against the best price
    pub fn is_marketable(&self, best_price: Option<f64>) -> bool {
        match (self.side, self.order_type, self.price, best_price) {
            // Market orders are always marketable if there's a price
            (_, OrderType::Market, _, Some(_)) => true,

            // Buy limit order is marketable if limit price >= best ask
            (Side::Buy, OrderType::Limit, Some(limit), Some(ask)) if limit >= ask => true,

            // Sell limit order is marketable if limit price <= best bid
            (Side::Sell, OrderType::Limit, Some(limit), Some(bid)) if limit <= bid => true,

            // Other cases are not marketable
            _ => false,
        }
    }
}

/// Trade resulting from matching orders
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: Uuid,
    pub symbol: String,
    pub buy_order_id: Uuid,
    pub sell_order_id: Uuid,
    pub price: f64,
    pub quantity: u64,
    pub timestamp: DateTime<Utc>,
}

impl Trade {
    pub fn new(
        symbol: String,
        buy_order_id: Uuid,
        sell_order_id: Uuid,
        price: f64,
        quantity: u64,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            symbol,
            buy_order_id,
            sell_order_id,
            price,
            quantity,
            timestamp: Utc::now(),
        }
    }
}

/// Messages passed between components of the exchange
#[derive(Debug, Clone)]
pub enum ExchangeMessage {
    /// Submit a new order to the exchange
    SubmitOrder(Order),

    /// Cancel an existing order
    CancelOrder(Uuid),

    /// Notification of order status update
    OrderUpdate {
        order_id: Uuid,
        status: OrderStatus,
        filled_qty: u64,
        symbol: String,
    },

    /// Notification of a trade
    Trade(Trade),

    /// Periodic heartbeat for time-based operations
    Heartbeat(DateTime<Utc>),
}
