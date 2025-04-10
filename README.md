# Rust Trading Exchange

A high-performance, multi-threaded trading exchange implementation in Rust, featuring a central limit order book with efficient order matching.

## Features

- **Multi-threaded Architecture**: Each order book runs in its own thread
- **Asynchronous Processing**: Built with Tokio for efficient async I/O
- **Configurable Time Management**: Adjustable clock speeds for testing
- **Comprehensive Order Types**:
  - Market Orders
  - Limit Orders
  - Stop Loss Orders
  - Stop Limit Orders
- **Time-in-Force Enforcement**:
  - IOC (Immediate or Cancel)
  - FOK (Fill or Kill)
  - GTC (Good Till Canceled)
  - GTD (Good Till Date)
  - DAY (Day orders)
- **Efficient Data Structures**: Optimized for high-frequency trading
- **Error Handling**: Robust error management with thiserror

## Architecture

The exchange is structured into several key components:

1. **Exchange**: Coordinates all operations and manages time
2. **Router**: Directs orders to the appropriate order book threads
3. **Order Book**: Maintains the order book for a specific symbol
4. **Matching Engine**: Handles the core order matching logic
5. **Time Management**: Configurable clock for testing

Communication between components is handled via Tokio channels.

## Getting Started

### Prerequisites

- Rust 1.56 or higher
- Cargo

### Installation

```bash
git clone https://github.com/yourusername/rust-exchange.git
cd rust-exchange
cargo build --release
```

### Running the Exchange

```bash
cargo run --release
```

## Usage Examples

### Creating a Basic Order

```rust
use rust_exchange::model::{Order, Side, OrderType, TimeInForce};

// Create a limit buy order
let order = Order::new(
    "BTC/USD".to_string(),
    Side::Buy,
    OrderType::Limit,
    100,             // Quantity
    Some(10000.0),   // Price
    None,            // Stop price (for stop orders)
    TimeInForce::GTC, // Good till canceled
);

// Submit to exchange
exchange.submit_order(order).await?;
```

### Using Different Time-in-Force Types

```rust
// Immediate or Cancel (IOC) - Execute immediately or cancel
let ioc_order = Order::new(
    "BTC/USD".to_string(),
    Side::Buy,
    OrderType::Limit,
    100,
    Some(10000.0),
    None,
    TimeInForce::IOC,
);

// Fill or Kill (FOK) - Fill completely or cancel
let fok_order = Order::new(
    "BTC/USD".to_string(),
    Side::Buy,
    OrderType::Limit,
    100,
    Some(10000.0),
    None,
    TimeInForce::FOK,
);

// Good Till Date (GTD) - Valid until specific date
let gtd_order = Order::new(
    "BTC/USD".to_string(),
    Side::Buy,
    OrderType::Limit,
    100,
    Some(10000.0),
    None,
    TimeInForce::GTD(Utc::now() + Duration::days(1)),
);
```

### Testing with Time Acceleration

```rust
// Set fixed time for testing
exchange.set_time_scale(TimeScale::Fixed)?;

// Submit GTD order that expires in 30 minutes
let gtd_order = Order::new(
    "BTC/USD".to_string(),
    Side::Buy,
    OrderType::Limit,
    100,
    Some(10000.0),
    None,
    TimeInForce::GTD(Utc::now() + Duration::minutes(30)),
);
exchange.submit_order(gtd_order).await?;

// Fast forward time by 1 hour to trigger expiration
exchange.advance_time(Duration::hours(1))?;

// Return to normal time
exchange.set_time_scale(TimeScale::Normal)?;
```

## Performance Considerations

- The exchange uses BTreeMap for price levels with vectors for time priority
- For HFT applications, consider further optimizations:
  - Lock-free data structures
  - SIMD for order matching
  - Memory pooling for order allocation
  - Custom allocators

## License

This project is licensed under the MIT License - see the LICENSE file for details.