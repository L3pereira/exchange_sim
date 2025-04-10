use chrono::{Duration, Utc};
use tokio::sync::mpsc::{channel, Receiver};
use tokio::time::sleep;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use crate::error::Result;
use crate::exchange::Exchange;
use crate::model::{ExchangeMessage, Order, OrderStatus, OrderType, Side, TimeInForce, Trade};
use crate::time::TimeScale;

// Helper function to collect and process messages
async fn collect_messages(rx: &mut Receiver<ExchangeMessage>, duration_ms: u64) -> Vec<ExchangeMessage> {
    let mut results = Vec::new();
    let timeout = tokio::time::Duration::from_millis(duration_ms);
    
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        tokio::select! {
            Some(msg) = rx.recv() => {
                results.push(msg);
            }
            _ = sleep(tokio::time::Duration::from_millis(1)) => {}
        }
    }
    
    results
}

#[tokio::test]
async fn test_limit_order_matching() -> Result<()> {
    // Create exchange
    let symbols = vec!["TEST/USD".to_string()];
    let channel_capacity = 100;
    let (client_tx, mut client_rx) = channel::<ExchangeMessage>(channel_capacity);
    
    let exchange = Arc::new(
        Exchange::new(
            symbols,
            client_tx,
            50, // Heartbeat interval
            channel_capacity,
        )
        .await?
    );
    
    // Create buy order
    let buy_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );
    
    // Create matching sell order
    let sell_order = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );
    
    // Submit buy order
    let buy_id = exchange.submit_order(buy_order).await?;
    
    // Collect messages for buy order
    let buy_msgs = collect_messages(&mut client_rx, 100).await;
    assert!(buy_msgs.len() >= 1, "Expected at least one message for buy order");
    
    // Submit sell order which should match
    let sell_id = exchange.submit_order(sell_order).await?;
    
    // Collect messages for matching
    let match_msgs = collect_messages(&mut client_rx, 100).await;
    
    // Verify we got trade and order updates
    let has_trade = match_msgs.iter().any(|msg| {
        if let ExchangeMessage::Trade(trade) = msg {
            trade.buy_order_id == buy_id && trade.sell_order_id == sell_id
        } else {
            false
        }
    });
    
    let buy_filled = match_msgs.iter().any(|msg| {
        if let ExchangeMessage::OrderUpdate { order_id, status, .. } = msg {
            *order_id == buy_id && *status == OrderStatus::Filled
        } else {
            false
        }
    });
    
    let sell_filled = match_msgs.iter().any(|msg| {
        if let ExchangeMessage::OrderUpdate { order_id, status, .. } = msg {
            *order_id == sell_id && *status == OrderStatus::Filled
        } else {
            false
        }
    });
    
    assert!(has_trade, "Expected a trade message");
    assert!(buy_filled, "Expected buy order to be filled");
    assert!(sell_filled, "Expected sell order to be filled");
    
    Ok(())
}

#[tokio::test]
async fn test_ioc_order() -> Result<()> {
    // Create exchange
    let symbols = vec!["TEST/USD".to_string()];
    let channel_capacity = 100;
    let (client_tx, mut client_rx) = channel::<ExchangeMessage>(channel_capacity);
    
    let exchange = Arc::new(
        Exchange::new(
            symbols,
            client_tx,
            50, // Heartbeat interval
            channel_capacity,
        )
        .await?
    );
    
    // Create resting sell order
    let sell_order = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );
    
    // Create IOC buy order for partial fill
    let ioc_buy_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        200, // More than available
        Some(100.0),
        None,
        TimeInForce::IOC,
    );
    
    // Submit sell order
    exchange.submit_order(sell_order).await?;
    
    // Clear messages
    collect_messages(&mut client_rx, 100).await;
    
    // Submit IOC buy order
    let ioc_id = exchange.submit_order(ioc_buy_order).await?;
    
    // Collect messages
    let ioc_msgs = collect_messages(&mut client_rx, 100).await;
    
    // IOC order should be partially filled but not completely
    let ioc_status = ioc_msgs.iter()
        .filter_map(|msg| {
            if let ExchangeMessage::OrderUpdate { order_id, status, filled_qty, .. } = msg {
                if *order_id == ioc_id {
                    Some((*status, *filled_qty))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .last();
    
    assert!(ioc_status.is_some(), "Expected IOC order update");
    if let Some((status, filled_qty)) = ioc_status {
        assert_eq!(filled_qty, 100, "Expected 100 units filled");
        assert_eq!(status, OrderStatus::PartiallyFilled, "Expected IOC order to be partially filled");
    }
    
    Ok(())
}

#[tokio::test]
async fn test_time_expiration() -> Result<()> {
    // Create exchange
    let symbols = vec!["TEST/USD".to_string()];
    let channel_capacity = 100;
    let (client_tx, mut client_rx) = channel::<ExchangeMessage>(channel_capacity);
    
    let exchange = Arc::new(
        Exchange::new(
            symbols,
            client_tx,
            50, // Heartbeat interval
            channel_capacity,
        )
        .await?
    );
    
    // Set fixed time
    exchange.set_time_scale(TimeScale::Fixed)?;
    
    // Create GTD order that expires in 5 minutes
    let gtd_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        100,
        Some(90.0),
        None,
        TimeInForce::GTD(Utc::now() + Duration::minutes(5)),
    );
    
    // Submit GTD order
    let gtd_id = exchange.submit_order(gtd_order).await?;
    
    // Clear initial messages
    collect_messages(&mut client_rx, 100).await;
    
    // Advance time by 10 minutes
    exchange.advance_time(Duration::minutes(10))?;
    
    // Send manual heartbeat to trigger expiration check
    sleep(StdDuration::from_millis(200)).await;
    
    // Collect messages
    let expire_msgs = collect_messages(&mut client_rx, 200).await;
    
    // Verify order was canceled due to expiration
    let order_expired = expire_msgs.iter().any(|msg| {
        if let ExchangeMessage::OrderUpdate { order_id, status, .. } = msg {
            *order_id == gtd_id && (*status == OrderStatus::Canceled || *status == OrderStatus::Expired)
        } else {
            false
        }
    });
    
    assert!(order_expired, "Expected GTD order to be expired");
    
    // Reset time scale
    exchange.set_time_scale(TimeScale::Normal)?;
    
    Ok(())
}

#[tokio::test]
async fn test_fok_order() -> Result<()> {
    // Create exchange
    let symbols = vec!["TEST/USD".to_string()];
    let channel_capacity = 100;
    let (client_tx, mut client_rx) = channel::<ExchangeMessage>(channel_capacity);
    
    let exchange = Arc::new(
        Exchange::new(
            symbols,
            client_tx,
            50, // Heartbeat interval
            channel_capacity,
        )
        .await?
    );
    
    // Create resting sell order
    let sell_order = Order::new(
        "TEST/USD".to_string(),
        Side::Sell,
        OrderType::Limit,
        100,
        Some(100.0),
        None,
        TimeInForce::GTC,
    );
    
    // Create FOK buy order that requires more than available
    let fok_buy_order = Order::new(
        "TEST/USD".to_string(),
        Side::Buy,
        OrderType::Limit,
        200, // More than available
        Some(100.0),
        None,
        TimeInForce::FOK,
    );
    
    // Submit sell order
    exchange.submit_order(sell_order).await?;
    
    // Clear messages
    collect_messages(&mut client_rx, 100).await;
    
    // Submit FOK buy order
    let fok_id = exchange.submit_order(fok_buy_order).await?;
    
    // Collect messages
    let fok_msgs = collect_messages(&mut client_rx, 100).await;
    
    // FOK order should be rejected
    let fok_rejected = fok_msgs.iter().any(|msg| {
        if let ExchangeMessage::OrderUpdate { order_id, status, .. } = msg {
            *order_id == fok_id && (*status == OrderStatus::Rejected || *status == OrderStatus::Canceled)
        } else {
            false
        }
    });
    
    assert!(fok_rejected, "Expected FOK order to be rejected");
    
    Ok(())
}