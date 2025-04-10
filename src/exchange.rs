use chrono::{Duration, Utc};
use log::{debug, error, info};
use std::sync::Arc;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::time::interval;
use uuid::Uuid;

use crate::error::{ExchangeError, Result};
use crate::model::{ExchangeMessage, Order};
use crate::router::Router;
use crate::time::{ExchangeClock, TimeScale};

/// Main exchange that coordinates all operations
pub struct Exchange {
    /// Available trading symbols
    symbols: Vec<String>,

    /// Shared exchange clock
    clock: Arc<ExchangeClock>,

    /// Channel to the router
    router_tx: Sender<ExchangeMessage>,

    /// Channel to clients (for notifications)
    client_tx: Sender<ExchangeMessage>,

    /// Heartbeat interval in milliseconds
    #[allow(dead_code)]
    heartbeat_interval_ms: u64,

    /// Reference to the router for configuration
    router: Arc<Router>,
}

impl Exchange {
    /// Create a new exchange instance
    pub async fn new(
        symbols: Vec<String>,
        client_tx: Sender<ExchangeMessage>,
        heartbeat_interval_ms: u64,
        channel_capacity: usize,
        default_matching_algorithm: String,
    ) -> Result<Self> {
        // Initialize the exchange clock
        let clock = Arc::new(ExchangeClock::new(None));

        // Create channels
        let (router_tx, router_rx) = channel::<ExchangeMessage>(channel_capacity);
        let (exchange_tx, exchange_rx) = channel::<ExchangeMessage>(channel_capacity);

        // Initialize router
        let router = Arc::new(Router::new(
            exchange_tx.clone(),
            clock.clone(),
            channel_capacity,
            default_matching_algorithm,
        ));

        // Create order books for each symbol
        for symbol in &symbols {
            router.create_order_book(symbol.clone())?;
        }

        // Spawn router task
        let router_clone = router.clone();

        tokio::spawn(async move {
            router_clone.run(router_rx).await;
        });

        // Spawn exchange message handler task
        let client_tx_clone = client_tx.clone();
        tokio::spawn(async move {
            Exchange::process_exchange_messages(exchange_rx, client_tx_clone).await;
        });

        // Start heartbeat
        let router_tx_clone = router_tx.clone();
        let clock_clone = clock.clone();
        tokio::spawn(async move {
            Exchange::start_heartbeat(router_tx_clone, clock_clone, heartbeat_interval_ms).await;
        });

        Ok(Self {
            symbols,
            clock,
            router_tx,
            client_tx,
            heartbeat_interval_ms,
            router,
        })
    }

    /// Start the periodic heartbeat task
    async fn start_heartbeat(
        tx: Sender<ExchangeMessage>,
        clock: Arc<ExchangeClock>,
        interval_ms: u64,
    ) {
        info!("Starting heartbeat with interval of {}ms", interval_ms);

        let mut tick_interval = interval(tokio::time::Duration::from_millis(interval_ms));

        loop {
            tick_interval.tick().await;

            // Get current time
            match clock.now().await {
                Ok(current_time) => {
                    // Send heartbeat
                    if let Err(e) = tx.send(ExchangeMessage::Heartbeat(current_time)).await {
                        error!("Error sending heartbeat: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    error!("Error getting current time: {}", e);
                }
            }
        }
    }

    /// Process messages coming back from order books via the router
    async fn process_exchange_messages(
        mut rx: Receiver<ExchangeMessage>,
        client_tx: Sender<ExchangeMessage>,
    ) {
        while let Some(message) = rx.recv().await {
            // Forward message to clients
            match &message {
                ExchangeMessage::OrderUpdate {
                    order_id,
                    status,
                    filled_qty,
                    symbol,
                } => {
                    debug!(
                        "Order update: id={}, symbol={}, status={:?}, filled={}",
                        order_id, symbol, status, filled_qty
                    );
                }
                ExchangeMessage::Trade(trade) => {
                    debug!(
                        "Trade executed: id={}, symbol={}, buy_order={}, sell_order={}, price={}, qty={}",
                        trade.id,
                        trade.symbol,
                        trade.buy_order_id,
                        trade.sell_order_id,
                        trade.price,
                        trade.quantity
                    );
                }
                _ => {}
            }

            if let Err(e) = client_tx.send(message).await {
                error!("Failed to forward message to client: {}", e);
            }
        }
    }

    /// Submit a new order to the exchange
    pub async fn submit_order(&self, order: Order) -> Result<Uuid> {
        // Validate symbol before sending
        if !self.symbols.contains(&order.symbol) {
            return Err(ExchangeError::SymbolNotFound(order.symbol));
        }

        let order_id = order.id;

        info!(
            "Submitting order: id={}, symbol={}, side={:?}, type={:?}, price={:?}",
            order.id, order.symbol, order.side, order.order_type, order.price
        );

        self.router_tx
            .send(ExchangeMessage::SubmitOrder(order))
            .await
            .map_err(|e| ExchangeError::ChannelSendError(e.to_string()))?;

        Ok(order_id)
    }

    /// Cancel an existing order
    pub async fn cancel_order(&self, order_id: Uuid) -> Result<()> {
        info!("Canceling order: id={}", order_id);

        self.router_tx
            .send(ExchangeMessage::CancelOrder(order_id))
            .await
            .map_err(|e| ExchangeError::ChannelSendError(e.to_string()))?;

        Ok(())
    }

    /// Set the clock's time scale for testing
    pub async fn set_time_scale(&self, scale: TimeScale) -> Result<()> {
        info!("Setting time scale to {:?}", scale);
        self.clock.set_time_scale(scale).await
    }

    /// Advance time (for Fixed time scale)
    pub async fn advance_time(&self, duration: Duration) -> Result<()> {
        info!(
            "Advancing time by {} milliseconds",
            duration.num_milliseconds()
        );
        self.clock.advance_time(duration).await
    }

    /// Get the current exchange time
    pub async fn current_time(&self) -> Result<chrono::DateTime<Utc>> {
        self.clock.now().await
    }

    /// Get available symbols
    pub fn symbols(&self) -> &[String] {
        &self.symbols
    }

    /// Set matching algorithm for a specific symbol
    pub fn set_matching_algorithm(&self, symbol: String, algorithm: String) -> Result<()> {
        if !self.symbols.contains(&symbol) {
            return Err(ExchangeError::SymbolNotFound(symbol));
        }

        self.router.set_matching_algorithm(symbol, algorithm);
        Ok(())
    }

    // Method to adjust heartbeat interval at runtime
    pub fn set_heartbeat_interval(&mut self, _new_interval_ms: u64) -> Result<()> {
        // self.heartbeat_interval_ms = 0;
        // Implementation to restart heartbeat task...
        todo!();
    }

    // Method to send custom messages to clients
    pub fn notify_clients(&self, message: ExchangeMessage) -> Result<()> {
        self.client_tx
            .try_send(message)
            .map_err(|e| ExchangeError::ChannelSendError(e.to_string()))?;
        Ok(())
    }
}
