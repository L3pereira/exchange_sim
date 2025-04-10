use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::channel;
use tokio::time::timeout;

use crate::error::Result;
use crate::matching_engine::PriceTimeMatchingEngine;
use crate::model::{ExchangeMessage, Order, OrderStatus, OrderType, Side, TimeInForce};
use crate::order_book::OrderBook;
use crate::time::ExchangeClock;

#[tokio::test]
async fn test_order_book_add_limit_order() -> Result<()> {
    // Setup test environment
    super::init_logger();
    let symbol = "TEST/USD".to_string();
    let clock = Arc::new(ExchangeClock::new(None));
    let (tx, mut _rx) = channel::<ExchangeMessage>(100);
    let matching_engine = Box::new(PriceTimeMatchingEngine::new());

    let mut order_book = OrderBook::new(symbol.clone(), clock, tx, matching_engine);

    // Create a limit order
    let limit_order = Order::new(
        symbol,
        Side::Buy,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );

    // Process the order
    order_book.process_order(limit_order.clone()).await?;

    // Verify the order was added (through best_bid inspection)
    assert_eq!(order_book.best_bid(), Some(100.0));

    Ok(())
}

#[tokio::test]
async fn test_order_book_match_orders() -> Result<()> {
    // Setup test environment
    super::init_logger();
    let symbol = "TEST/USD".to_string();
    let clock = Arc::new(ExchangeClock::new(None));
    let (tx, mut rx) = channel::<ExchangeMessage>(100);
    let matching_engine = Box::new(PriceTimeMatchingEngine::new());

    let mut order_book = OrderBook::new(symbol.clone(), clock, tx, matching_engine);

    // Create a resting limit order
    let limit_order = Order::new(
        symbol.clone(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );

    // Add the resting order
    order_book.process_order(limit_order.clone()).await?;

    // Create a matching market order
    let market_order = Order::new(
        symbol,
        Side::Buy,
        OrderType::Market,
        50, // Partially fill the resting order
        None,
        None,
        TimeInForce::GTC,
    );

    // Process the market order
    order_book.process_order(market_order.clone()).await?;

    // Collect messages to check results
    let mut received_order_update = false;
    let mut received_trade = false;

    // Wait for messages with a timeout
    for _ in 0..5 {
        match timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(msg)) => {
                match msg {
                    ExchangeMessage::OrderUpdate {
                        status, filled_qty, ..
                    } => {
                        received_order_update = true;
                        if status == OrderStatus::PartiallyFilled && filled_qty == 50 {
                            // Resting order partially filled
                        }
                        if status == OrderStatus::Filled && filled_qty == 50 {
                            // Market order fully filled
                        }
                    }
                    ExchangeMessage::Trade(trade) => {
                        received_trade = true;
                        assert_eq!(trade.quantity, 50);
                        assert_eq!(trade.price, 100.0);
                    }
                    _ => {}
                }
            }
            _ => break,
        }
    }

    assert!(received_order_update);
    assert!(received_trade);

    Ok(())
}

#[tokio::test]
async fn test_order_book_cancel_order() -> Result<()> {
    // Setup test environment
    super::init_logger();
    let symbol = "TEST/USD".to_string();
    let clock = Arc::new(ExchangeClock::new(None));
    let (tx, mut rx) = channel::<ExchangeMessage>(100);
    let matching_engine = Box::new(PriceTimeMatchingEngine::new());

    let mut order_book = OrderBook::new(symbol.clone(), clock, tx, matching_engine);

    // Create a limit order
    let limit_order = Order::new(
        symbol,
        Side::Buy,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );

    // Add the order
    order_book.process_order(limit_order.clone()).await?;

    // Cancel the order
    order_book.cancel_order(limit_order.id).await?;

    // Verify best bid is gone
    assert_eq!(order_book.best_bid(), None);

    // Check for cancel message
    let mut received_cancel = false;

    // Wait for messages with a timeout
    match timeout(Duration::from_millis(100), rx.recv()).await {
        Ok(Some(ExchangeMessage::OrderUpdate {
            status, order_id, ..
        })) => {
            received_cancel = status == OrderStatus::Canceled && order_id == limit_order.id;
        }
        _ => {}
    }

    assert!(received_cancel);

    Ok(())
}

#[tokio::test]
async fn test_order_book_time_in_force() -> Result<()> {
    // Setup test environment
    super::init_logger();
    let symbol = "TEST/USD".to_string();
    let clock = Arc::new(ExchangeClock::new(None));
    let (tx, mut rx) = channel::<ExchangeMessage>(100);
    let matching_engine = Box::new(PriceTimeMatchingEngine::new());

    let mut order_book = OrderBook::new(symbol.clone(), clock.clone(), tx, matching_engine);

    // IOC order with no match
    let ioc_order = Order::new(
        symbol.clone(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(90.0), // Too low to match anything
        None,
        TimeInForce::IOC,
    );

    // Process the IOC order
    order_book.process_order(ioc_order.clone()).await?;

    // Verify it was canceled
    let mut received_cancel = false;

    match timeout(Duration::from_millis(100), rx.recv()).await {
        Ok(Some(ExchangeMessage::OrderUpdate {
            status, order_id, ..
        })) => {
            received_cancel = status == OrderStatus::Canceled && order_id == ioc_order.id;
        }
        _ => {}
    }

    assert!(received_cancel);

    // FOK order with no match
    let fok_order = Order::new(
        symbol.clone(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(90.0), // Too low to match anything
        None,
        TimeInForce::FOK,
    );

    // Process the FOK order
    order_book.process_order(fok_order.clone()).await?;

    // Verify it was rejected
    let mut received_reject = false;

    match timeout(Duration::from_millis(100), rx.recv()).await {
        Ok(Some(ExchangeMessage::OrderUpdate {
            status, order_id, ..
        })) => {
            received_reject = status == OrderStatus::Rejected && order_id == fok_order.id;
        }
        _ => {}
    }

    assert!(received_reject);

    Ok(())
}

#[tokio::test]
async fn test_order_book_stop_orders() -> Result<()> {
    // Setup test environment
    super::init_logger();
    let symbol = "TEST/USD".to_string();
    let clock = Arc::new(ExchangeClock::new(None));
    let (tx, mut rx) = channel::<ExchangeMessage>(100);
    let matching_engine = Box::new(PriceTimeMatchingEngine::new());

    let mut order_book = OrderBook::new(symbol.clone(), clock.clone(), tx, matching_engine);

    // Add a resting sell order to create market price
    let sell_order = Order::new(
        symbol.clone(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );

    order_book.process_order(sell_order.clone()).await?;

    // Clear messages
    while let Ok(Some(_)) = timeout(Duration::from_millis(50), rx.recv()).await {}

    // Create a buy stop order with stop price above current best ask
    let stop_order = Order::new(
        symbol.clone(),
        Side::Buy,
        OrderType::StopLoss,
        50,
        None,
        Some(101.0), // Stop price higher than current ask
        TimeInForce::GTC,
    );

    // Process the stop order - should not trigger yet
    order_book.process_order(stop_order.clone()).await?;

    // Create another sell order at a higher price that will trigger the stop
    let trigger_sell = Order::new(
        symbol.clone(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(98.0), // Lower price, will trigger stop
        None,
        TimeInForce::GTC,
    );

    order_book.process_order(trigger_sell.clone()).await?;

    // In a real system, price updates would trigger stop orders
    // For this test, we would check if stop order is still in the book or got triggered

    Ok(())
}
