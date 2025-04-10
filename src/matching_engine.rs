use std::collections::HashMap;

use crate::error::{ExchangeError, Result};
use crate::model::{Order, Side, TimeInForce, Trade};

/// Defines the interface for different matching algorithms
pub trait MatchingAlgorithm {
    /// Check if two orders can match
    fn can_match(&self, buy_order: &Order, sell_order: &Order) -> bool;

    /// Match two orders and return the resulting trade and remaining quantities
    fn match_orders(&mut self, buy_order: &Order, sell_order: &Order) -> Result<(Trade, u64, u64)>;

    /// Get the last traded price for a symbol
    fn last_price(&self, symbol: &str) -> Option<f64>;

    /// Update the last traded price for a symbol
    fn update_last_price(&mut self, symbol: &str, price: f64);

    /// Validate if an order can be filled according to time-in-force rules
    fn validate_time_in_force(&self, order: &Order, fill_qty: u64) -> Result<bool>;

    /// Get the name of the algorithm
    fn name(&self) -> &str;
}

/// Standard price-time priority matching engine
pub struct PriceTimeMatchingEngine {
    /// Last trade price per symbol
    last_prices: HashMap<String, f64>,
}

impl PriceTimeMatchingEngine {
    pub fn new() -> Self {
        Self {
            last_prices: HashMap::new(),
        }
    }
}

impl MatchingAlgorithm for PriceTimeMatchingEngine {
    fn name(&self) -> &str {
        "Price-Time Priority"
    }

    /// Get the last trade price for a symbol
    fn last_price(&self, symbol: &str) -> Option<f64> {
        self.last_prices.get(symbol).copied()
    }

    /// Update the last trade price for a symbol
    fn update_last_price(&mut self, symbol: &str, price: f64) {
        self.last_prices.insert(symbol.to_string(), price);
    }

    /// Determine if two orders can match
    fn can_match(&self, buy_order: &Order, sell_order: &Order) -> bool {
        // Must be same symbol
        if buy_order.symbol != sell_order.symbol {
            return false;
        }

        // Must be opposite sides
        if buy_order.side != Side::Buy || sell_order.side != Side::Sell {
            return false;
        }

        // Check price conditions
        match (buy_order.price, sell_order.price) {
            // Both are limit orders - price must cross
            (Some(buy_price), Some(sell_price)) => buy_price >= sell_price,

            // At least one is a market order - can match
            _ => true,
        }
    }

    /// Match two orders and generate trades
    /// Returns the trade and remaining quantities for both orders
    fn match_orders(&mut self, buy_order: &Order, sell_order: &Order) -> Result<(Trade, u64, u64)> {
        if !self.can_match(buy_order, sell_order) {
            return Err(ExchangeError::InternalError(
                "Orders cannot match".to_string(),
            ));
        }

        // Calculate match quantity
        let buy_remaining = buy_order.quantity - buy_order.filled_quantity;
        let sell_remaining = sell_order.quantity - sell_order.filled_quantity;
        let match_qty = std::cmp::min(buy_remaining, sell_remaining);

        if match_qty == 0 {
            return Err(ExchangeError::InternalError(
                "No quantity to match".to_string(),
            ));
        }

        // Determine match price according to price-time priority
        let match_price = match (buy_order.price, sell_order.price) {
            // Both limit orders - resting order sets the price
            (Some(buy_price), Some(sell_price)) => {
                if buy_order.created_at < sell_order.created_at {
                    buy_price
                } else {
                    sell_price
                }
            }

            // Buy is market order, use sell limit price
            (None, Some(sell_price)) => sell_price,

            // Sell is market order, use buy limit price
            (Some(buy_price), None) => buy_price,

            // Both are market orders, use last price or midpoint
            (None, None) => {
                // In a real system, we would use more sophisticated price discovery
                self.last_price(&buy_order.symbol).unwrap_or(0.0)
            }
        };

        // Create trade record
        let trade = Trade::new(
            buy_order.symbol.clone(),
            buy_order.id,
            sell_order.id,
            match_price,
            match_qty,
        );

        // Update last price
        self.update_last_price(&trade.symbol, match_price);

        // Calculate remaining quantities
        let buy_remaining_after = buy_remaining - match_qty;
        let sell_remaining_after = sell_remaining - match_qty;

        Ok((trade, buy_remaining_after, sell_remaining_after))
    }

    /// Validate if an order can be filled according to time-in-force rules
    fn validate_time_in_force(&self, order: &Order, fill_qty: u64) -> Result<bool> {
        match order.time_in_force {
            // Fill or Kill - must be fully filled or not at all
            TimeInForce::FOK => {
                if fill_qty < order.quantity {
                    return Err(ExchangeError::TimeInForceViolation(
                        "FOK order cannot be partially filled".to_string(),
                    ));
                }
                Ok(true)
            }

            // Immediate or Cancel - can be partially filled but rest is canceled
            TimeInForce::IOC => {
                // IOC can always be executed, partial fills are allowed
                Ok(true)
            }

            // Other time-in-force types don't have special execution rules
            _ => Ok(true),
        }
    }
}

/// Pro-rata matching engine implementation
pub struct ProRataMatchingEngine {
    /// Last trade price per symbol
    last_prices: HashMap<String, f64>,
}

impl ProRataMatchingEngine {
    pub fn new() -> Self {
        Self {
            last_prices: HashMap::new(),
        }
    }
}

impl MatchingAlgorithm for ProRataMatchingEngine {
    fn name(&self) -> &str {
        "Pro-Rata"
    }

    fn last_price(&self, symbol: &str) -> Option<f64> {
        self.last_prices.get(symbol).copied()
    }

    fn update_last_price(&mut self, symbol: &str, price: f64) {
        self.last_prices.insert(symbol.to_string(), price);
    }

    fn can_match(&self, buy_order: &Order, sell_order: &Order) -> bool {
        // Same matching criteria as price-time
        if buy_order.symbol != sell_order.symbol {
            return false;
        }

        if buy_order.side != Side::Buy || sell_order.side != Side::Sell {
            return false;
        }

        match (buy_order.price, sell_order.price) {
            (Some(buy_price), Some(sell_price)) => buy_price >= sell_price,
            _ => true,
        }
    }

    fn match_orders(&mut self, buy_order: &Order, sell_order: &Order) -> Result<(Trade, u64, u64)> {
        // For single order matching, pro-rata behaves like price-time
        // The difference would be seen when matching against multiple orders

        if !self.can_match(buy_order, sell_order) {
            return Err(ExchangeError::InternalError(
                "Orders cannot match".to_string(),
            ));
        }

        let buy_remaining = buy_order.quantity - buy_order.filled_quantity;
        let sell_remaining = sell_order.quantity - sell_order.filled_quantity;
        let match_qty = std::cmp::min(buy_remaining, sell_remaining);

        if match_qty == 0 {
            return Err(ExchangeError::InternalError(
                "No quantity to match".to_string(),
            ));
        }

        // Price determination is the same
        let match_price = match (buy_order.price, sell_order.price) {
            (Some(buy_price), Some(sell_price)) => {
                if buy_order.created_at < sell_order.created_at {
                    buy_price
                } else {
                    sell_price
                }
            }
            (None, Some(sell_price)) => sell_price,
            (Some(buy_price), None) => buy_price,
            (None, None) => self.last_price(&buy_order.symbol).unwrap_or(0.0),
        };

        let trade = Trade::new(
            buy_order.symbol.clone(),
            buy_order.id,
            sell_order.id,
            match_price,
            match_qty,
        );

        self.update_last_price(&trade.symbol, match_price);

        // Calculate remaining quantities
        let buy_remaining_after = buy_remaining - match_qty;
        let sell_remaining_after = sell_remaining - match_qty;

        Ok((trade, buy_remaining_after, sell_remaining_after))
    }

    fn validate_time_in_force(&self, order: &Order, fill_qty: u64) -> Result<bool> {
        // Same TIF rules apply regardless of matching algorithm
        match order.time_in_force {
            TimeInForce::FOK => {
                if fill_qty < order.quantity {
                    return Err(ExchangeError::TimeInForceViolation(
                        "FOK order cannot be partially filled".to_string(),
                    ));
                }
                Ok(true)
            }
            TimeInForce::IOC => Ok(true),
            _ => Ok(true),
        }
    }
}

/// Factory function to create matching algorithms
pub fn create_matching_algorithm(algorithm_type: &str) -> Box<dyn MatchingAlgorithm + Send> {
    match algorithm_type.to_lowercase().as_str() {
        "pro-rata" => Box::new(ProRataMatchingEngine::new()),
        // Add other algorithm types here
        _ => Box::new(PriceTimeMatchingEngine::new()), // Default
    }
}
