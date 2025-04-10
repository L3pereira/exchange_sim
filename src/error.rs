use thiserror::Error;

#[derive(Error, Debug)]
pub enum ExchangeError {
    #[error("Invalid order: {0}")]
    InvalidOrder(String),

    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    #[error("Order not found: {0}")]
    OrderNotFound(String),

    #[error("Insufficient liquidity")]
    InsufficientLiquidity,

    #[error("Time-in-force violation: {0}")]
    TimeInForceViolation(String),

    #[error("Channel send error: {0}")]
    ChannelSendError(String),

    #[error("Channel receive error: {0}")]
    ChannelReceiveError(String),

    #[error("Time error: {0}")]
    TimeError(String),

    #[error("Internal error: {0}")]
    InternalError(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ExchangeError>;
