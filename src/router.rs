use dashmap::DashMap;
use log::{debug, error, info, trace};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender, channel};

use crate::error::{ExchangeError, Result};
use crate::matching_engine::create_matching_algorithm;
use crate::model::ExchangeMessage;
use crate::order_book::OrderBook;
use crate::time::ExchangeClock;

/// Router directs messages to the appropriate order book thread
pub struct Router {
    /// Channel senders for each order book, keyed by symbol
    order_book_tx: DashMap<String, Sender<ExchangeMessage>>,

    /// Channel to send notifications back to the exchange
    exchange_tx: Sender<ExchangeMessage>,

    /// Shared exchange clock
    clock: Arc<ExchangeClock>,

    /// Channel capacity for order book channels
    channel_capacity: usize,

    /// Default matching algorithm type
    default_matching_algorithm: String,

    /// Symbol-specific matching algorithms
    symbol_matching_algorithms: DashMap<String, String>,
}

impl Router {
    pub fn new(
        exchange_tx: Sender<ExchangeMessage>,
        clock: Arc<ExchangeClock>,
        channel_capacity: usize,
        default_matching_algorithm: String,
    ) -> Self {
        Self {
            order_book_tx: DashMap::new(),
            exchange_tx,
            clock,
            channel_capacity,
            default_matching_algorithm,
            symbol_matching_algorithms: DashMap::new(),
        }
    }

    /// Set a specific matching algorithm for a symbol
    pub fn set_matching_algorithm(&self, symbol: String, algorithm: String) {
        self.symbol_matching_algorithms.insert(symbol, algorithm);
    }

    /// Get the matching algorithm for a symbol
    fn get_matching_algorithm(&self, symbol: &str) -> String {
        self.symbol_matching_algorithms
            .get(symbol)
            .map(|algo| algo.clone())
            .unwrap_or_else(|| self.default_matching_algorithm.clone())
    }

    /// Create a new order book thread for a symbol
    pub fn create_order_book(&self, symbol: String) -> Result<()> {
        // Check if order book already exists
        if self.order_book_tx.contains_key(&symbol) {
            return Err(ExchangeError::InternalError(format!(
                "Order book for {} already exists",
                symbol
            )));
        }

        // Get the matching algorithm for this symbol
        let algo_type = self.get_matching_algorithm(&symbol);
        let matching_engine = create_matching_algorithm(&algo_type);

        info!(
            "Creating order book for symbol: {} with matching algorithm: {}",
            symbol,
            matching_engine.name()
        );

        // Create channel for the order book
        let (tx, rx) = channel::<ExchangeMessage>(self.channel_capacity);

        // Create order book
        let order_book = OrderBook::new(
            symbol.clone(),
            self.clock.clone(),
            self.exchange_tx.clone(),
            matching_engine,
        );

        // Spawn a task for the order book
        let ob_symbol = symbol.clone();
        tokio::spawn(async move {
            debug!("Order book thread started for symbol: {}", ob_symbol);
            order_book.run(rx).await;
        });

        // Store the sender
        self.order_book_tx.insert(symbol, tx);

        Ok(())
    }

    /// Route a message to the appropriate order book
    pub async fn route_message(&self, message: ExchangeMessage) -> Result<()> {
        match &message {
            ExchangeMessage::SubmitOrder(order) => {
                trace!(
                    "route_message received SubmitOrder : id={}, symbol={}, side={:?}",
                    order.id, order.symbol, order.side
                );

                let symbol = &order.symbol;
                // Ensure order book exists
                if !self.order_book_tx.contains_key(symbol) {
                    return Err(ExchangeError::SymbolNotFound(symbol.clone()));
                }

                // Get the channel for this symbol
                if let Some(tx) = self.order_book_tx.get(symbol) {
                    tx.send(message)
                        .await
                        .map_err(|e| ExchangeError::ChannelSendError(e.to_string()))?;
                } else {
                    return Err(ExchangeError::SymbolNotFound(symbol.clone()));
                }
            }

            ExchangeMessage::CancelOrder(order_id) => {
                trace!("route_message received CancelOrder : id={}", order_id);
                // In a real system, we'd track which order is in which book
                // For simplicity, we broadcast to all books
                let mut sent = false;

                for item in self.order_book_tx.iter() {
                    let tx = item.value();
                    if let Err(e) = tx.send(message.clone()).await {
                        error!("Failed to send cancel order to book: {}", e);
                    } else {
                        sent = true;
                    }
                }

                if !sent {
                    return Err(ExchangeError::OrderNotFound(order_id.to_string()));
                }
            }

            ExchangeMessage::Heartbeat(_) => {
                // Broadcast heartbeat to all order books
                for item in self.order_book_tx.iter() {
                    let tx = item.value();
                    if let Err(e) = tx.send(message.clone()).await {
                        error!("Failed to send heartbeat to book: {}", e);
                    }
                }
            }

            _ => {
                // Forward other messages to the exchange
                self.exchange_tx
                    .send(message)
                    .await
                    .map_err(|e| ExchangeError::ChannelSendError(e.to_string()))?;
            }
        }

        Ok(())
    }

    /// Run the router message processing loop
    pub async fn run(&self, mut rx: Receiver<ExchangeMessage>) {
        info!(
            "Router starting with default matching algorithm: {}",
            self.default_matching_algorithm
        );

        while let Some(message) = rx.recv().await {
            trace!("Message Received from Exchange {:?}", message);
            if let Err(e) = self.route_message(message).await {
                error!("Error routing message: {}", e);
            }
        }

        info!("Router shutting down");
    }
}
