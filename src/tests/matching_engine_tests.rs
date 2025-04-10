use chrono::Utc;
use uuid::Uuid;

use crate::error::Result;
use crate::matching_engine::{MatchingAlgorithm, PriceTimeMatchingEngine, ProRataMatchingEngine};
use crate::model::{Order, OrderType, Side, TimeInForce};

// Helper function to create test orders
fn create_test_orders() -> (Order, Order) {
    let buy_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );

    let sell_order = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );

    (buy_order, sell_order)
}

#[test]
fn test_price_time_matching_can_match() -> Result<()> {
    let engine = PriceTimeMatchingEngine::new();
    let (buy_order, sell_order) = create_test_orders();

    // Same symbol, opposite sides, matching prices - should match
    assert!(engine.can_match(&buy_order, &sell_order));

    // Different symbol - should not match
    let mut different_symbol_sell = sell_order.clone();
    different_symbol_sell.symbol = "OTHER/USD".to_string();
    assert!(!engine.can_match(&buy_order, &different_symbol_sell));

    // Same side - should not match
    let mut same_side_order = buy_order.clone();
    same_side_order.id = Uuid::new_v4(); // Different ID
    assert!(!engine.can_match(&buy_order, &same_side_order));

    // Non-matching price - should not match
    let mut higher_price_sell = sell_order.clone();
    higher_price_sell.price = Some(101.0);
    assert!(!engine.can_match(&buy_order, &higher_price_sell));

    // Market order - should always match with limit order
    let mut market_buy = buy_order.clone();
    market_buy.order_type = OrderType::Market;
    market_buy.price = None;
    assert!(engine.can_match(&market_buy, &sell_order));

    Ok(())
}

#[test]
fn test_price_time_matching_match_orders() -> Result<()> {
    let mut engine = PriceTimeMatchingEngine::new();
    let (buy_order, sell_order) = create_test_orders();

    // Full match
    let result = engine.match_orders(&buy_order, &sell_order)?;
    let (trade, buy_remaining, sell_remaining) = result;

    // Both orders should be fully matched
    assert_eq!(buy_remaining, 0);
    assert_eq!(sell_remaining, 0);
    assert_eq!(trade.quantity, 100);
    assert_eq!(trade.price, 100.0);
    assert_eq!(trade.buy_order_id, buy_order.id);
    assert_eq!(trade.sell_order_id, sell_order.id);

    // Partial match - buy order larger
    let mut large_buy = buy_order.clone();
    large_buy.quantity = 200;
    let result = engine.match_orders(&large_buy, &sell_order)?;
    let (trade, buy_remaining, sell_remaining) = result;

    assert_eq!(buy_remaining, 100); // 200 - 100 = 100 remaining
    assert_eq!(sell_remaining, 0); // Fully matched
    assert_eq!(trade.quantity, 100);

    // Partial match - sell order larger
    let mut large_sell = sell_order.clone();
    large_sell.quantity = 150;
    let result = engine.match_orders(&buy_order, &large_sell)?;
    let (trade, buy_remaining, sell_remaining) = result;

    assert_eq!(buy_remaining, 0); // Fully matched
    assert_eq!(sell_remaining, 50); // 150 - 100 = 50 remaining
    assert_eq!(trade.quantity, 100);

    Ok(())
}

#[test]
fn test_price_time_price_determination() -> Result<()> {
    let mut engine = PriceTimeMatchingEngine::new();

    // Test resting order price (time priority)
    let mut early_buy = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );
    early_buy.created_at = Utc::now() - chrono::Duration::seconds(10);

    let sell_order = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(99.0), // Lower price
        None,
        TimeInForce::GTC,
    );

    // Earlier order sets the price
    let result = engine.match_orders(&early_buy, &sell_order)?;
    assert_eq!(result.0.price, 100.0); // Buy price as it was resting

    // Market order with limit order
    let market_buy = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Market,
        100,
        None,
        None,
        TimeInForce::GTC,
    );

    let result = engine.match_orders(&market_buy, &sell_order)?;
    assert_eq!(result.0.price, 99.0); // Sell limit price used for market buy

    Ok(())
}

#[test]
fn test_pro_rata_matching() -> Result<()> {
    let mut engine = ProRataMatchingEngine::new();
    let (buy_order, sell_order) = create_test_orders();

    // Pro-rata should still match identical orders
    assert!(engine.can_match(&buy_order, &sell_order));

    let result = engine.match_orders(&buy_order, &sell_order)?;
    let (trade, buy_remaining, sell_remaining) = result;

    // Both orders should be fully matched
    assert_eq!(buy_remaining, 0);
    assert_eq!(sell_remaining, 0);
    assert_eq!(trade.quantity, 100);

    // Last price should be updated
    assert_eq!(engine.last_price(&buy_order.symbol), Some(100.0));

    Ok(())
}

#[test]
fn test_time_in_force_validation() -> Result<()> {
    let engine = PriceTimeMatchingEngine::new();

    // FOK - full fill
    let fok_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::FOK,
    );

    assert!(engine.validate_time_in_force(&fok_order, 100).is_ok());

    // FOK - partial fill should be rejected
    let result = engine.validate_time_in_force(&fok_order, 50);
    assert!(result.is_err());

    // IOC - partial fill is fine
    let ioc_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::IOC,
    );

    assert!(engine.validate_time_in_force(&ioc_order, 50).is_ok());

    Ok(())
}
