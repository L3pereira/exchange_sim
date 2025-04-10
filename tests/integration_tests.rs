use chrono::{DateTime, Duration, Utc};
use env_logger::Env;
use log::{debug, info};
use rust_decimal_macros::dec;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration as StdDuration;
use tokio::sync::mpsc::{Receiver, channel};
use tokio::time::timeout;

use exchange_sim::error::Result;
use exchange_sim::exchange::Exchange;
use exchange_sim::model::{
    ExchangeMessage, Order, OrderStatus, OrderType, Side, TimeInForce, Trade,
};
use exchange_sim::time::TimeScale;

// This makes sure we only initialize the logger once
static INIT: Once = Once::new();

fn init_logger() {
    INIT.call_once(|| {
        env_logger::Builder::from_env(Env::default().filter_or("RUST_LOG", "debug"))
            .format_timestamp_millis()
            .init();
    });
}

// Helper function to collect messages from the exchange
async fn collect_messages(
    rx: &mut Receiver<ExchangeMessage>,
    duration_ms: u64,
) -> Vec<ExchangeMessage> {
    let mut messages = Vec::new();
    let timeout_duration = StdDuration::from_millis(duration_ms);

    info!("Starting to collect messages for {}ms", duration_ms);

    loop {
        match timeout(timeout_duration, rx.recv()).await {
            Ok(Some(msg)) => {
                debug!("Received message: {:?}", msg);
                messages.push(msg);
            }
            Ok(None) => {
                debug!("Channel closed");
                break;
            }
            Err(_) => {
                debug!("Timeout waiting for message");
                break;
            }
        }
    }
    debug!("Collected {} messages", messages.len());
    messages
}

// Helper to extract order updates from messages
fn extract_order_updates(messages: &[ExchangeMessage]) -> HashMap<uuid::Uuid, OrderStatus> {
    let mut updates = HashMap::new();

    for msg in messages {
        if let ExchangeMessage::OrderUpdate {
            order_id, status, ..
        } = msg
        {
            updates.insert(*order_id, *status);
        }
    }

    updates
}

// Helper to extract trades from messages
fn extract_trades(messages: &[ExchangeMessage]) -> Vec<Trade> {
    let mut trades = Vec::new();

    for msg in messages {
        if let ExchangeMessage::Trade(trade) = msg {
            trades.push(trade.clone());
        }
    }

    trades
}

#[tokio::test]
async fn test_complete_trading_scenario() -> Result<()> {
    init_logger();
    // Setup exchange
    let symbols = vec!["TEST/USD".to_string()];
    let (client_tx, mut client_rx) = channel::<ExchangeMessage>(100);

    let exchange = Arc::new(
        Exchange::new(
            symbols.clone(),
            client_tx,
            50,  // Fast heartbeat for testing
            100, // Small channel capacity for testing
            "price-time".to_string(),
        )
        .await?,
    );

    // 1. Submit resting orders to build the book

    // Sell side
    let sell_order_1 = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        dec!(100),
        Some(dec!(10000.0)),
        None,
        TimeInForce::GTC,
    );

    let sell_order_2 = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        dec!(200),
        Some(dec!(10100.0)),
        None,
        TimeInForce::GTC,
    );

    // Buy side
    let buy_order_1 = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        dec!(150),
        Some(dec!(9900.0)),
        None,
        TimeInForce::GTC,
    );

    let buy_order_2 = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        dec!(250),
        Some(dec!(9800.0)),
        None,
        TimeInForce::GTC,
    );

    // Submit the orders
    let sell_id_1 = exchange.submit_order(sell_order_1).await?;
    let _sell_id_2 = exchange.submit_order(sell_order_2).await?;
    let buy_id_1 = exchange.submit_order(buy_order_1).await?;
    let _buy_id_2 = exchange.submit_order(buy_order_2).await?;

    // Collect messages to clear the channel
    let _ = collect_messages(&mut client_rx, 100).await;

    // 2. Submit a marketable order that will match
    let market_buy = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Market,
        dec!(50),
        None,
        None,
        TimeInForce::IOC,
    );

    let market_id = exchange.submit_order(market_buy).await?;

    // Collect messages
    let messages = collect_messages(&mut client_rx, 1000).await;

    // Verify we got the expected updates
    let updates = extract_order_updates(&messages);
    let trades = extract_trades(&messages);

    // Market buy should be filled
    assert_eq!(updates.get(&market_id), Some(&OrderStatus::Filled));

    // Sell order 1 should be partially filled
    assert_eq!(updates.get(&sell_id_1), Some(&OrderStatus::PartiallyFilled));

    // Should have one trade
    assert_eq!(trades.len(), 1);
    assert_eq!(trades[0].quantity, dec!(50));
    assert_eq!(trades[0].buy_order_id, market_id);
    assert_eq!(trades[0].sell_order_id, sell_id_1);

    // 3. Test time-based order expiration

    // Set fixed time for testing
    exchange.set_time_scale(TimeScale::Fixed).await?;

    // Add a GTD order that will expire
    let gtd_order = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        dec!(300),
        Some(dec!(10200.0)),
        None,
        TimeInForce::GTD(Utc::now() + Duration::seconds(5)),
    );

    let gtd_id = exchange.submit_order(gtd_order).await?;

    // Clear messages
    let _ = collect_messages(&mut client_rx, 100).await;

    // Advance time to trigger expiration
    exchange.advance_time(Duration::seconds(10)).await?;

    // Need to wait a bit for the heartbeat and processing
    tokio::time::sleep(StdDuration::from_millis(200)).await;

    // Collect messages
    let messages = collect_messages(&mut client_rx, 100).await;
    let updates = extract_order_updates(&messages);

    // GTD order should be canceled/expired
    assert!(
        updates
            .get(&gtd_id)
            .map_or(false, |&s| s == OrderStatus::Canceled)
    );

    // Reset time scale
    exchange.set_time_scale(TimeScale::Normal).await?;

    // 4. Test FOK order that fails

    // Clear messages
    let _ = collect_messages(&mut client_rx, 100).await;

    // Submit FOK order that's too large to fill
    let fok_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        dec!(1000), // Larger than available quantity at this price
        Some(dec!(10000.0)),
        None,
        TimeInForce::FOK,
    );

    let fok_id = exchange.submit_order(fok_order).await?;

    // Collect messages
    let messages = collect_messages(&mut client_rx, 100).await;
    let updates = extract_order_updates(&messages);

    // FOK order should be rejected
    assert!(
        updates
            .get(&fok_id)
            .map_or(false, |&s| s == OrderStatus::Rejected)
    );

    // 5. Test order cancellation

    // Clear messages
    let _ = collect_messages(&mut client_rx, 100).await;

    // Cancel one of the resting orders
    exchange.cancel_order(buy_id_1).await?;

    // Collect messages
    let messages = collect_messages(&mut client_rx, 100).await;
    let updates = extract_order_updates(&messages);

    // Buy order should be canceled
    assert!(
        updates
            .get(&buy_id_1)
            .map_or(false, |&s| s == OrderStatus::Canceled)
    );

    Ok(())
}

#[tokio::test]
async fn test_multiple_matching_algorithms() -> Result<()> {
    // Setup exchange with two symbols
    let symbols = vec!["PRICE_TIME/USD".to_string(), "PRO_RATA/USD".to_string()];
    let (client_tx, mut client_rx) = channel::<ExchangeMessage>(100);

    let exchange = Arc::new(
        Exchange::new(
            symbols.clone(),
            client_tx,
            50,
            100,
            "price-time".to_string(), // Default algorithm
        )
        .await?,
    );

    // Set pro-rata algorithm for second symbol
    exchange.set_matching_algorithm("PRO_RATA/USD".to_string(), "pro-rata".to_string())?;

    // Clear initial messages
    let _ = collect_messages(&mut client_rx, 100).await;

    // Setup identical books on both symbols
    for symbol in &symbols {
        // Sell side
        let sell_order = Order::new(
            symbol.clone(),
            Side::Sell,
            OrderType::Limit,
            dec!(100),
            Some(dec!(100.0)),
            None,
            TimeInForce::GTC,
        );

        exchange.submit_order(sell_order).await?;

        // Buy side
        let buy_order = Order::new(
            symbol.clone(),
            Side::Buy,
            OrderType::Limit,
            dec!(100),
            Some(dec!(90.0)),
            None,
            TimeInForce::GTC,
        );

        exchange.submit_order(buy_order).await?;
    }

    // Clear initial order messages
    let _ = collect_messages(&mut client_rx, 100).await;

    // Submit marketable orders to both symbols
    for symbol in &symbols {
        let market_order = Order::new(
            symbol.clone(),
            Side::Buy,
            OrderType::Market,
            dec!(50),
            None,
            None,
            TimeInForce::GTC,
        );

        exchange.submit_order(market_order).await?;

        // Allow time for processing
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }

    // Collect all messages
    let messages = collect_messages(&mut client_rx, 100).await;

    // Group trades by symbol
    let mut trades_by_symbol = HashMap::new();

    for message in &messages {
        if let ExchangeMessage::Trade(trade) = message {
            let entry = trades_by_symbol
                .entry(trade.symbol.clone())
                .or_insert_with(Vec::new);
            entry.push(trade.clone());
        }
    }

    // Verify both symbols had trades
    assert!(trades_by_symbol.contains_key("PRICE_TIME/USD"));
    assert!(trades_by_symbol.contains_key("PRO_RATA/USD"));

    // Both should have the same number of trades in this simple case
    assert_eq!(
        trades_by_symbol.get("PRICE_TIME/USD").unwrap().len(),
        trades_by_symbol.get("PRO_RATA/USD").unwrap().len()
    );

    Ok(())
}

#[tokio::test]
async fn test_error_handling() -> Result<()> {
    // init_logger();
    // Setup exchange
    let symbols = vec!["TEST/USD".to_string()];
    let (client_tx, mut client_rx) = channel::<ExchangeMessage>(100);

    let exchange = Arc::new(
        Exchange::new(
            symbols.clone(),
            client_tx,
            50,
            100,
            "price-time".to_string(),
        )
        .await?,
    );

    // 1. Submit order with invalid symbol
    let invalid_symbol_order = Order::new(
        "INVALID/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        dec!(100),
        Some(dec!(100.0)),
        None,
        TimeInForce::GTC,
    );

    // This should return an error
    let result = exchange.submit_order(invalid_symbol_order).await;
    assert!(result.is_err());

    // 2. Submit order with invalid parameters (limit order without price)
    let invalid_limit_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        dec!(100),
        None, // Missing price
        None,
        TimeInForce::GTC,
    );

    // Submit should succeed, but processing should fail
    let order_id = exchange.submit_order(invalid_limit_order).await?;

    // Collect messages
    let messages = collect_messages(&mut client_rx, 100).await;
    let updates = extract_order_updates(&messages);

    // Verify rejection
    assert!(
        updates
            .get(&order_id)
            .map_or(false, |&s| s == OrderStatus::Rejected)
    );

    // 3. Try to cancel non-existent order
    let random_id = uuid::Uuid::new_v4();
    let _ = exchange.cancel_order(random_id).await;

    let messages = collect_messages(&mut client_rx, 100).await;
    let updates = extract_order_updates(&messages);
    assert!(
        updates
            .get(&random_id)
            .map_or(false, |&s| s == OrderStatus::Rejected)
    );

    Ok(())
}

#[tokio::test]
async fn test_time_management() -> Result<()> {
    // Setup exchange
    let symbols = vec!["TEST/USD".to_string()];
    let (client_tx, mut _client_rx) = channel::<ExchangeMessage>(100);

    let exchange = Arc::new(
        Exchange::new(
            symbols.clone(),
            client_tx,
            50,
            100,
            "price-time".to_string(),
        )
        .await?,
    );

    // Test normal time
    let start_time = exchange.current_time().await?;

    // Sleep a bit
    tokio::time::sleep(StdDuration::from_millis(100)).await;

    // Time should have advanced
    let current_time = exchange.current_time().await?;
    assert!(current_time > start_time);

    // Test fixed time
    exchange.set_time_scale(TimeScale::Fixed).await?;
    let fixed_time = exchange.current_time().await?;

    // Sleep a bit
    tokio::time::sleep(StdDuration::from_millis(100)).await;

    // Time should not have advanced
    assert_eq!(exchange.current_time().await?, fixed_time);

    // Manually advance time
    exchange.advance_time(Duration::seconds(60)).await?;

    // Time should have advanced by exactly 60 seconds
    let new_time = exchange.current_time().await?;
    assert_eq!(new_time, fixed_time + Duration::seconds(60));

    // Test fast time
    exchange.set_time_scale(TimeScale::Fast(10)).await?;
    let fast_start = exchange.current_time().await?;

    // Sleep a bit
    tokio::time::sleep(StdDuration::from_millis(100)).await;

    // Time should have advanced faster than real time
    let fast_current = exchange.current_time().await?;
    let real_elapsed =
        Utc::now() - DateTime::<Utc>::from_timestamp(fast_start.timestamp(), 0).unwrap();
    let sim_elapsed = fast_current - fast_start;

    // Not a precise test, but sim_elapsed should be roughly 10x real_elapsed
    // We're just checking that it's significantly faster
    assert!(sim_elapsed > real_elapsed * 2);

    Ok(())
}
