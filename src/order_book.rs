use chrono::Utc;
use log::{debug, error, info, trace, warn};
use priority_queue::PriorityQueue;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

use crate::error::{ExchangeError, Result};
use crate::matching_engine::MatchingAlgorithm;
use crate::model::{ExchangeMessage, Order, OrderStatus, OrderType, Side, TimeInForce};
use crate::time::ExchangeClock;

/// Priority wrapper for buy orders (higher price first, then older orders)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct BuyPriority(i64, Reverse<u64>); // (price_key, Reverse(sequence))

/// Priority wrapper for sell orders (lower price first, then older orders)
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SellPriority(Reverse<i64>, Reverse<u64>); // (Reverse(price_key), Reverse(sequence))

/// Limit order book with price-time priority using priority queue
pub struct OrderBook {
    symbol: String,

    // Buy orders: highest price first, then oldest first
    bids: PriorityQueue<Uuid, BuyPriority>,

    // Sell orders: lowest price first, then oldest first
    asks: PriorityQueue<Uuid, SellPriority>,

    // Map to quickly look up orders by ID
    orders: HashMap<Uuid, Order>,

    // Map order ID to its side and price
    order_info: HashMap<Uuid, (Side, i64)>,

    // For tracking total quantity at each price level
    bid_quantities: HashMap<i64, u64>,
    ask_quantities: HashMap<i64, u64>,

    // Next sequence number for time priority
    next_sequence: u64,

    // Exchange clock for time-dependent operations
    clock: Arc<ExchangeClock>,

    // Channel to communicate with router
    router_tx: Sender<ExchangeMessage>,

    // Matching algorithm
    matching_engine: Box<dyn MatchingAlgorithm + Send>,
}

impl OrderBook {
    pub fn new(
        symbol: String,
        clock: Arc<ExchangeClock>,
        router_tx: Sender<ExchangeMessage>,
        matching_engine: Box<dyn MatchingAlgorithm + Send>,
    ) -> Self {
        info!("Creating new OrderBook for symbol: {}", symbol);
        Self {
            symbol,
            bids: PriorityQueue::new(),
            asks: PriorityQueue::new(),
            orders: HashMap::new(),
            order_info: HashMap::new(),
            bid_quantities: HashMap::new(),
            ask_quantities: HashMap::new(),
            next_sequence: 1,
            clock,
            router_tx,
            matching_engine,
        }
    }

    // Convert float price to integer for precision-safe comparison
    fn price_to_key(price: f64) -> i64 {
        (price * 10000.0) as i64
    }

    // Convert integer key back to float price
    fn key_to_price(key: i64) -> f64 {
        key as f64 / 10000.0
    }

    // Get the best bid price
    pub fn best_bid(&self) -> Option<f64> {
        self.bids
            .peek()
            .map(|(_, priority)| Self::key_to_price(priority.0))
    }

    // Get the best ask price
    pub fn best_ask(&self) -> Option<f64> {
        self.asks.peek().map(|(_, priority)| {
            // Extract the price from SellPriority(Reverse(price), _)
            let SellPriority(Reverse(price_key), _) = priority;
            Self::key_to_price(*price_key)
        })
    }

    // Get total quantity available at a price level
    fn get_quantity_at_price(&self, side: Side, price_key: i64) -> u64 {
        match side {
            Side::Buy => *self.bid_quantities.get(&price_key).unwrap_or(&0),
            Side::Sell => *self.ask_quantities.get(&price_key).unwrap_or(&0),
        }
    }

    // Update the quantity for a price level
    fn update_quantity_at_price(&mut self, side: Side, price_key: i64, quantity: u64) {
        match side {
            Side::Buy => {
                if quantity > 0 {
                    self.bid_quantities.insert(price_key, quantity);
                } else {
                    self.bid_quantities.remove(&price_key);
                }
            }
            Side::Sell => {
                if quantity > 0 {
                    self.ask_quantities.insert(price_key, quantity);
                } else {
                    self.ask_quantities.remove(&price_key);
                }
            }
        }
    }

    // Helper to get priority info for an order
    // fn get_order_priority_seq(&self, side: Side, order_id: &Uuid) -> Option<u64> {
    //     match side {
    //         Side::Buy => self.bids.get_priority(order_id)
    //             .map(|BuyPriority(_, Reverse(seq))| *seq),
    //         Side::Sell => self.asks.get_priority(order_id)
    //             .map(|SellPriority(_, Reverse(seq))| *seq),
    //     }
    // }

    // Helper to pop the best order by side
    fn pop_best_order(&mut self, side: Side) -> Option<Uuid> {
        match side {
            Side::Buy => self.asks.pop().map(|(id, _)| id),
            Side::Sell => self.bids.pop().map(|(id, _)| id),
        }
    }

    // Helper to simplify quantity calculation
    fn get_available_quantity(&self, side: Side) -> u64 {
        match side {
            Side::Buy => self.ask_quantities.values().sum(),
            Side::Sell => self.bid_quantities.values().sum(),
        }
    }

    // Add a limit order to the book
    fn add_limit_order(&mut self, mut order: Order) -> Result<()> {
        trace!(
            "Adding limit order to book: id={}, symbol={}, side={:?}",
            order.id, order.symbol, order.side
        );

        let price = match order.price {
            Some(p) => p,
            None => {
                error!(
                    "Attempted to add limit order without price: id={}",
                    order.id
                );
                return Err(ExchangeError::InvalidOrder(
                    "Limit orders must have a price".to_string(),
                ));
            }
        };

        let price_key = Self::price_to_key(price);

        // Update order status and timestamps
        order.status = OrderStatus::New;
        order.updated_at = Utc::now();

        // Get sequence number for time priority
        let sequence = self.next_sequence;
        self.next_sequence += 1;

        // Add to appropriate priority queue based on side
        match order.side {
            Side::Buy => {
                self.bids
                    .push(order.id, BuyPriority(price_key, Reverse(sequence)));

                // Update quantity tracking
                let current_qty = self.get_quantity_at_price(Side::Buy, price_key);
                let new_qty = current_qty + (order.quantity - order.filled_quantity);
                self.update_quantity_at_price(Side::Buy, price_key, new_qty);
            }
            Side::Sell => {
                self.asks.push(
                    order.id,
                    SellPriority(Reverse(price_key), Reverse(sequence)),
                );

                // Update quantity tracking
                let current_qty = self.get_quantity_at_price(Side::Sell, price_key);
                let new_qty = current_qty + (order.quantity - order.filled_quantity);
                self.update_quantity_at_price(Side::Sell, price_key, new_qty);
            }
        }

        // Store order in lookups
        self.order_info.insert(order.id, (order.side, price_key));
        self.orders.insert(order.id, order.clone());

        debug!(
            "Added limit order: id={}, symbol={}, side={:?}, price={}, qty={}",
            order.id, order.symbol, order.side, price, order.quantity
        );

        Ok(())
    }

    // Remove an order from the book
    fn remove_order(&mut self, order_id: Uuid) -> Result<Option<Order>> {
        trace!("Attempting to remove order: id={}", order_id);

        if let Some((side, price_key)) = self.order_info.remove(&order_id) {
            trace!(
                "Found order in map: side={:?}, price_key={}",
                side, price_key
            );

            // Remove from appropriate priority queue - just get the ID
            let removed = match side {
                Side::Buy => self.bids.remove(&order_id).map(|(uuid, _)| uuid),
                Side::Sell => self.asks.remove(&order_id).map(|(uuid, _)| uuid),
            };

            if removed.is_some() {
                // Update quantity tracking for the price level
                if let Some(order) = self.orders.get(&order_id) {
                    let order_qty = order.quantity - order.filled_quantity;
                    let current_qty = self.get_quantity_at_price(side, price_key);
                    let new_qty = current_qty.saturating_sub(order_qty);
                    self.update_quantity_at_price(side, price_key, new_qty);
                }

                // Get the order from storage
                let order = self.orders.remove(&order_id);

                if let Some(ref order) = order {
                    debug!(
                        "Removed order: id={}, symbol={}, side={:?}, price={:?}",
                        order_id, order.symbol, order.side, order.price
                    );
                }

                return Ok(order);
            } else {
                error!(
                    "Order id={} was in order_info but not found in priority queue",
                    order_id
                );
            }
        } else {
            trace!("Order not found in order_info map: id={}", order_id);
        }

        Ok(None)
    }

    // Process a market order - immediate execution at current market prices
    async fn process_market_order(&mut self, mut order: Order) -> Result<()> {
        debug!(
            "PROCESSING MARKET ORDER: id={}, symbol={}, side={:?}, qty={}, TIF={:?}",
            order.id, order.symbol, order.side, order.quantity, order.time_in_force
        );

        let mut remaining_qty = order.quantity;
        trace!("Initial remaining_qty: {}", remaining_qty);

        // Check if order can be filled according to time-in-force
        if matches!(order.time_in_force, TimeInForce::FOK) {
            // For FOK, check if we can fill the entire order
            let available_qty = self.get_available_quantity(order.side);

            trace!(
                "FOK validation: order qty={}, available qty={}",
                order.quantity, available_qty
            );

            // Validate using the matching engine's method
            match self
                .matching_engine
                .validate_time_in_force(&order, available_qty)
            {
                Ok(_) => {
                    trace!("FOK validation passed - proceeding with matching");
                }
                Err(e) => {
                    // Can't satisfy FOK - reject the order
                    order.status = OrderStatus::Rejected;

                    debug!(
                        "FOK validation failed - rejecting order: id={}, reason: {}",
                        order.id, e
                    );

                    if let Err(e) = self
                        .router_tx
                        .send(ExchangeMessage::OrderUpdate {
                            order_id: order.id,
                            status: OrderStatus::Rejected,
                            filled_qty: 0,
                            symbol: self.symbol.clone(),
                        })
                        .await
                    {
                        error!("Failed to send rejection notification: {}", e);
                        return Err(ExchangeError::ChannelSendError(e.to_string()));
                    }

                    return Err(e);
                }
            }
        }

        // Track orders we've matched against so we can reinsert partially filled orders
        let mut matched_orders: Vec<(Uuid, bool)> = Vec::new(); // (id, order, fully_filled)

        // Keep matching until we've filled the order or no more liquidity
        while remaining_qty > 0 {
            // Get the next best order from the opposite side using our helper
            if let Some(order_id) = self.pop_best_order(order.side) {
                if let Some(resting_order) = self.orders.get(&order_id).cloned() {
                    trace!(
                        "Matching against resting order: id={}, qty={}, filled={}",
                        resting_order.id, resting_order.quantity, resting_order.filled_quantity
                    );

                    // Use matching engine to match orders
                    let matched_result =
                        self.matching_engine.match_orders(&order, &resting_order)?;

                    // Get trade and remaining quantities
                    let (trade, incoming_remaining, resting_remaining) = match order.side {
                        Side::Buy => {
                            let (trade, buy_remaining, sell_remaining) = matched_result;
                            (trade, buy_remaining, sell_remaining)
                        }
                        Side::Sell => {
                            let (trade, sell_remaining, buy_remaining) = matched_result;
                            (trade, sell_remaining, buy_remaining)
                        }
                    };

                    trace!(
                        "Match result: trade qty={}, incoming_remaining={}, resting_remaining={}",
                        trade.quantity, incoming_remaining, resting_remaining
                    );

                    // Update quantities
                    remaining_qty = incoming_remaining;
                    order.filled_quantity = order.quantity - remaining_qty;

                    // Update resting order's filled quantity
                    let mut updated_resting = resting_order.clone();
                    updated_resting.filled_quantity = resting_order.quantity - resting_remaining;

                    // Update quantity tracking for the price level
                    let price_key = Self::price_to_key(resting_order.price.unwrap_or(trade.price));
                    let current_qty = self.get_quantity_at_price(resting_order.side, price_key);
                    let new_qty = current_qty - trade.quantity;
                    self.update_quantity_at_price(resting_order.side, price_key, new_qty);

                    // Send trade notification
                    if let Err(e) = self.router_tx.send(ExchangeMessage::Trade(trade)).await {
                        error!("Failed to send trade notification: {}", e);
                        return Err(ExchangeError::ChannelSendError(e.to_string()));
                    }

                    // Determine if resting order is fully filled
                    let fully_filled = updated_resting.filled_quantity == updated_resting.quantity;

                    if fully_filled {
                        updated_resting.status = OrderStatus::Filled;
                        self.order_info.remove(&updated_resting.id);
                        trace!("Resting order fully filled: id={}", updated_resting.id);

                        // Send filled notification
                        if let Err(e) = self
                            .router_tx
                            .send(ExchangeMessage::OrderUpdate {
                                order_id: updated_resting.id,
                                status: OrderStatus::Filled,
                                filled_qty: updated_resting.filled_quantity,
                                symbol: self.symbol.clone(),
                            })
                            .await
                        {
                            error!("Failed to send filled notification: {}", e);
                            return Err(ExchangeError::ChannelSendError(e.to_string()));
                        }
                    } else {
                        updated_resting.status = OrderStatus::PartiallyFilled;

                        trace!(
                            "Resting order partially filled: id={}, filled={}/{}",
                            updated_resting.id,
                            updated_resting.filled_quantity,
                            updated_resting.quantity
                        );

                        // Send partially filled notification
                        if let Err(e) = self
                            .router_tx
                            .send(ExchangeMessage::OrderUpdate {
                                order_id: updated_resting.id,
                                status: OrderStatus::PartiallyFilled,
                                filled_qty: updated_resting.filled_quantity,
                                symbol: self.symbol.clone(),
                            })
                            .await
                        {
                            error!("Failed to send partially filled notification: {}", e);
                            return Err(ExchangeError::ChannelSendError(e.to_string()));
                        }
                    }

                    // Update the order in storage
                    self.orders
                        .insert(updated_resting.id, updated_resting.clone());

                    // Keep track of this order for possible reinsertion
                    matched_orders.push((order_id, fully_filled));
                } else {
                    error!(
                        "Order id={} was in priority queue but not in orders map",
                        order_id
                    );
                }
            } else {
                // No more orders to match against
                break;
            }
        }

        // Reinsert partially filled orders back into the priority queue
        for (order_id, fully_filled) in matched_orders {
            if !fully_filled {
                // Get the original sequence number and price
                if let Some((side, price_key)) = self.order_info.get(&order_id) {
                    // Get a new sequence number - we can't reliably retrieve the original
                    let seq = self.next_sequence;
                    self.next_sequence += 1;

                    // Reinsert the order with the new sequence
                    match *side {
                        Side::Buy => {
                            self.bids
                                .push(order_id, BuyPriority(*price_key, Reverse(seq)));
                        }
                        Side::Sell => {
                            self.asks
                                .push(order_id, SellPriority(Reverse(*price_key), Reverse(seq)));
                        }
                    }
                }
            }
        }

        // Update incoming order status based on fill and time enforcement
        let final_status = match (order.filled_quantity, order.quantity, order.time_in_force) {
            // Fully filled
            (filled_qty, qty, _) if filled_qty == qty => {
                trace!("Order is fully filled: {} of {}", filled_qty, qty);
                OrderStatus::Filled
            }

            // IOC - partial fills allowed, cancel rest
            (filled_qty, qty, TimeInForce::IOC) if filled_qty < qty => {
                if filled_qty > 0 {
                    trace!(
                        "IOC order partially filled: {} of {}, canceling remainder",
                        filled_qty, qty
                    );
                    OrderStatus::PartiallyFilled
                } else {
                    trace!("IOC order not filled at all, canceling");
                    OrderStatus::Canceled
                }
            }

            // Partially filled
            (filled_qty, qty, _) if filled_qty < qty => {
                trace!("Order partially filled: {} of {}", filled_qty, qty);
                OrderStatus::PartiallyFilled
            }

            // Default (shouldn't happen)
            _ => {
                trace!("Unexpected order state, rejecting");
                OrderStatus::Rejected
            }
        };

        trace!(
            "Final order status: {:?}, filled_qty: {}",
            final_status, order.filled_quantity
        );

        // Send order update notification
        order.status = final_status;
        if let Err(e) = self
            .router_tx
            .send(ExchangeMessage::OrderUpdate {
                order_id: order.id,
                status: final_status,
                filled_qty: order.filled_quantity,
                symbol: self.symbol.clone(),
            })
            .await
        {
            error!("Failed to send order update notification: {}", e);
            return Err(ExchangeError::ChannelSendError(e.to_string()));
        }

        Ok(())
    }

    // Process a limit order - can execute immediately if price conditions are met
    async fn process_limit_order(&mut self, mut order: Order) -> Result<()> {
        let order_id = order.id;

        debug!(
            "PROCESSING LIMIT ORDER: id={}, symbol={}, side={:?}, price={:?}, qty={}",
            order.id, order.symbol, order.side, order.price, order.quantity
        );

        let is_marketable = match order.side {
            Side::Buy => {
                if let Some(best_ask) = self.best_ask() {
                    let marketable = order.price.unwrap_or(0.0) >= best_ask;
                    trace!(
                        "Buy limit order marketability check: order price={:?}, best ask={}, marketable={}",
                        order.price, best_ask, marketable
                    );
                    marketable
                } else {
                    trace!("No asks in the book - buy limit order is not marketable");
                    false
                }
            }
            Side::Sell => {
                if let Some(best_bid) = self.best_bid() {
                    let marketable = order.price.unwrap_or(f64::MAX) <= best_bid;
                    trace!(
                        "Sell limit order marketability check: order price={:?}, best bid={}, marketable={}",
                        order.price, best_bid, marketable
                    );
                    marketable
                } else {
                    trace!("No bids in the book - sell limit order is not marketable");
                    false
                }
            }
        };

        if is_marketable {
            trace!("Order is marketable - processing immediately");
            // Process the marketable portion like a market order
            self.process_market_order(order.clone()).await?;

            // Check if order should be added to book after partial execution
            if order.filled_quantity < order.quantity
                && order.status != OrderStatus::Canceled
                && order.status != OrderStatus::Rejected
            {
                trace!(
                    "Order partially filled: {}/{} - checking time-in-force constraints",
                    order.filled_quantity, order.quantity
                );

                // For IOC orders, cancel any remaining quantity
                if matches!(order.time_in_force, TimeInForce::IOC) {
                    order.status = if order.filled_quantity > 0 {
                        trace!("IOC order partially filled - canceling remainder");
                        OrderStatus::PartiallyFilled
                    } else {
                        trace!("IOC order not filled - canceling");
                        OrderStatus::Canceled
                    };

                    if let Err(e) = self
                        .router_tx
                        .send(ExchangeMessage::OrderUpdate {
                            order_id: order.id,
                            status: order.status,
                            filled_qty: order.filled_quantity,
                            symbol: self.symbol.clone(),
                        })
                        .await
                    {
                        error!("Failed to send IOC order update: {}", e);
                        return Err(ExchangeError::ChannelSendError(e.to_string()));
                    }
                } else {
                    // Add remaining quantity to the book
                    trace!("Adding remaining quantity to the book");
                    self.add_limit_order(order)?;
                }
            } else {
                trace!("Order fully processed - no remainder to add to book");
            }
        } else {
            // Not immediately marketable
            trace!("Order is not marketable - checking time-in-force constraints");

            // Validate time-in-force constraints
            match order.time_in_force {
                TimeInForce::IOC => {
                    // Cancel immediately since it can't be filled right now
                    trace!("IOC order can't be immediately executed - canceling");
                    order.status = OrderStatus::Canceled;
                    if let Err(e) = self
                        .router_tx
                        .send(ExchangeMessage::OrderUpdate {
                            order_id: order.id,
                            status: OrderStatus::Canceled,
                            filled_qty: 0,
                            symbol: self.symbol.clone(),
                        })
                        .await
                    {
                        error!("Failed to send IOC cancel notification: {}", e);
                        return Err(ExchangeError::ChannelSendError(e.to_string()));
                    }
                }
                TimeInForce::FOK => {
                    // Reject since it can't be filled completely right now
                    trace!("FOK order can't be immediately executed in full - rejecting");
                    order.status = OrderStatus::Rejected;
                    if let Err(e) = self
                        .router_tx
                        .send(ExchangeMessage::OrderUpdate {
                            order_id: order.id,
                            status: OrderStatus::Rejected,
                            filled_qty: 0,
                            symbol: self.symbol.clone(),
                        })
                        .await
                    {
                        error!("Failed to send FOK reject notification: {}", e);
                        return Err(ExchangeError::ChannelSendError(e.to_string()));
                    }
                }
                _ => {
                    // GTC, GTD, DAY - add to the book
                    trace!(
                        "Adding non-marketable order to book with TIF={:?}",
                        order.time_in_force
                    );
                    self.add_limit_order(order)?;
                }
            }
        }

        trace!("Limit order processing complete for id={}", order_id);

        Ok(())
    }

    // Process stop orders - trigger when price threshold is reached
    async fn process_stop_order(&mut self, order: Order) -> Result<()> {
        let stop_price = match order.stop_price {
            Some(price) => price,
            None => {
                error!("Stop order missing stop price: id={}", order.id);
                return Err(ExchangeError::InvalidOrder(
                    "Stop orders must have a stop price".to_string(),
                ));
            }
        };

        debug!(
            "PROCESSING STOP ORDER: id={}, symbol={}, side={:?}, stop_price={}, qty={}",
            order.id, order.symbol, order.side, stop_price, order.quantity
        );

        // Check if stop price has been triggered
        let triggered = match order.side {
            // Buy stop triggers when market price >= stop price
            Side::Buy => {
                if let Some(best_ask) = self.best_ask() {
                    let is_triggered = best_ask <= stop_price;
                    trace!(
                        "Buy stop check: stop price={}, best ask={}, triggered={}",
                        stop_price, best_ask, is_triggered
                    );
                    is_triggered
                } else {
                    trace!("No asks in book - buy stop not triggered");
                    false
                }
            }
            // Sell stop triggers when market price <= stop price
            Side::Sell => {
                if let Some(best_bid) = self.best_bid() {
                    let is_triggered = best_bid >= stop_price;
                    trace!(
                        "Sell stop check: stop price={}, best bid={}, triggered={}",
                        stop_price, best_bid, is_triggered
                    );
                    is_triggered
                } else {
                    trace!("No bids in book - sell stop not triggered");
                    false
                }
            }
        };

        if triggered {
            trace!(
                "Stop order triggered - converting to {}",
                match order.order_type {
                    OrderType::StopLoss => "market order",
                    OrderType::StopLimit => "limit order",
                    _ => "unknown order type",
                }
            );

            // Convert to market or limit order and process
            let mut triggered_order = order.clone();

            triggered_order.order_type = match order.order_type {
                OrderType::StopLoss => OrderType::Market,
                OrderType::StopLimit => OrderType::Limit,
                _ => {
                    error!("Invalid stop order type: {:?}", order.order_type);
                    return Err(ExchangeError::InvalidOrder(
                        "Invalid order type conversion".to_string(),
                    ));
                }
            };

            match triggered_order.order_type {
                OrderType::Market => self.process_market_order(triggered_order).await?,
                OrderType::Limit => self.process_limit_order(triggered_order).await?,
                _ => unreachable!(),
            }
        } else {
            // Store stop order for later triggering
            // In a real system, we'd maintain a separate collection for stop orders
            // and check them on every price update

            trace!("Stop order not triggered - storing for later activation");

            // For this implementation, we'll just send a "New" update
            if let Err(e) = self
                .router_tx
                .send(ExchangeMessage::OrderUpdate {
                    order_id: order.id,
                    status: OrderStatus::New,
                    filled_qty: 0,
                    symbol: self.symbol.clone(),
                })
                .await
            {
                error!("Failed to send stop order new notification: {}", e);
                return Err(ExchangeError::ChannelSendError(e.to_string()));
            }
        }

        trace!("Stop order processing complete for id={}", order.id);
        Ok(())
    }

    // Process an incoming order
    pub async fn process_order(&mut self, order: Order) -> Result<()> {
        let order_id = order.id;
        debug!(
            "Processing incoming order: id={}, symbol={}, type={:?}, side={:?}, qty={}",
            order.id, order.symbol, order.order_type, order.side, order.quantity
        );

        // Validate order
        if order.quantity == 0 {
            error!("Order quantity is zero: id={}", order.id);
            return Err(ExchangeError::InvalidOrder(
                "Order quantity must be greater than 0".to_string(),
            ));
        }

        if order.symbol != self.symbol {
            error!(
                "Symbol mismatch: order symbol={}, book symbol={}",
                order.symbol, self.symbol
            );
            return Err(ExchangeError::InvalidOrder(format!(
                "Symbol mismatch: expected {}, got {}",
                self.symbol, order.symbol
            )));
        }

        if !order.validate() {
            error!("Invalid order parameters: id={}", order.id);
            return Err(ExchangeError::InvalidOrder(
                "Invalid order parameters".to_string(),
            ));
        }

        trace!(
            "Order validated successfully, processing by type: {:?}",
            order.order_type
        );

        // Process based on order type
        match order.order_type {
            OrderType::Market => {
                self.process_market_order(order).await?;
            }
            OrderType::Limit => {
                self.process_limit_order(order).await?;
            }
            OrderType::StopLoss | OrderType::StopLimit => {
                self.process_stop_order(order).await?;
            }
        }

        debug!("Order processing complete: id={}", order_id);

        Ok(())
    }

    // Cancel an order
    pub async fn cancel_order(&mut self, order_id: Uuid) -> Result<()> {
        debug!("Canceling order: id={}", order_id);

        if let Some(order) = self.remove_order(order_id)? {
            debug!(
                "Order found and removed from book: id={}, symbol={}, side={:?}",
                order_id, order.symbol, order.side
            );

            if let Err(e) = self
                .router_tx
                .send(ExchangeMessage::OrderUpdate {
                    order_id,
                    status: OrderStatus::Canceled,
                    filled_qty: order.filled_quantity,
                    symbol: self.symbol.clone(),
                })
                .await
            {
                error!("Failed to send cancel notification: {}", e);
                return Err(ExchangeError::ChannelSendError(e.to_string()));
            }

            trace!("Cancel notification sent successfully");
            Ok(())
        } else {
            error!("Order not found for cancellation: id={}", order_id);
            Err(ExchangeError::OrderNotFound(order_id.to_string()))
        }
    }

    // Check for expired orders (GTD, DAY)
    pub async fn check_expired_orders(&mut self) -> Result<()> {
        trace!("Checking for expired orders");
        let now = self.clock.now().await?;
        let mut expired_orders = Vec::new();

        // We need to check both bids and asks
        for order_id in self.orders.keys() {
            if let Some(order) = self.orders.get(order_id) {
                match &order.time_in_force {
                    TimeInForce::GTD(expiry) if *expiry <= now => {
                        debug!(
                            "Order expired (GTD): id={}, expiry={:?}, now={:?}",
                            order.id, expiry, now
                        );
                        expired_orders.push(*order_id);
                    }
                    TimeInForce::DAY => {
                        // For DAY orders, we'd check against market close time
                        trace!("DAY order - would check against market close time");
                    }
                    _ => {
                        trace!("Non-expiring TIF: {:?}", order.time_in_force);
                    }
                }
            }
        }

        // Cancel expired orders
        debug!("Found {} expired orders to cancel", expired_orders.len());
        for order_id in expired_orders {
            debug!("Canceling expired order: id={}", order_id);
            self.cancel_order(order_id).await?;
        }

        trace!("Expired order check complete");

        Ok(())
    }

    // Run the order book processing loop
    pub async fn run(mut self, mut rx: Receiver<ExchangeMessage>) {
        info!(
            "Starting order book for symbol: {} with matching algorithm: {}",
            self.symbol,
            self.matching_engine.name()
        );

        // Create a reusable closure for sending rejection messages
        let send_rejection =
            |router_tx: &Sender<ExchangeMessage>, order_id: Uuid, symbol: String| {
                let router_tx = router_tx.clone();
                async move {
                    if let Err(send_err) = router_tx
                        .send(ExchangeMessage::OrderUpdate {
                            order_id,
                            status: OrderStatus::Rejected,
                            filled_qty: 0,
                            symbol,
                        })
                        .await
                    {
                        error!("Failed to send rejection: {}", send_err);
                    }
                }
            };

        while let Some(msg) = rx.recv().await {
            match msg {
                ExchangeMessage::SubmitOrder(order) => {
                    let order_id = order.id;
                    if let Err(e) = self.process_order(order).await {
                        error!("Error processing order: {}", e);
                        send_rejection(&self.router_tx, order_id, self.symbol.clone()).await;
                    }
                }
                ExchangeMessage::CancelOrder(order_id) => {
                    if let Err(e) = self.cancel_order(order_id).await {
                        error!("Error canceling order: {}", e);
                        send_rejection(&self.router_tx, order_id, self.symbol.clone()).await;
                    } else {
                        warn!("This was supposed to fail: {}", order_id);
                    }
                }
                ExchangeMessage::Heartbeat(_timestamp) => {
                    // Update internal time and check for expired orders
                    if let Err(e) = self.check_expired_orders().await {
                        error!("Error checking expired orders: {}", e);
                    }
                }
                _ => {} // Ignore other message types
            }
        }

        info!("Order book for symbol {} shutting down", self.symbol);
    }
}
