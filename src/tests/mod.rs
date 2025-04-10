// #[cfg(test)]
// mod exchange_tests;
use env_logger::Env;
use std::sync::Once;
// This makes sure we only initialize the logger once
#[allow(dead_code)]
static INIT: Once = Once::new();

#[allow(dead_code)]
fn init_logger() {
    INIT.call_once(|| {
        env_logger::Builder::from_env(Env::default().filter_or("RUST_LOG", "debug"))
            .format_timestamp_millis()
            .init();
    });
}

#[cfg(test)]
mod order_book_tests;

#[cfg(test)]
mod matching_engine_tests;
