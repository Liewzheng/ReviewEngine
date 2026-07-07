//! Entry point for the review-engine binary. Parses CLI arguments and dispatches to the appropriate command handler.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//!
//! @module review-engine
#[cfg(feature = "cli")]
mod cli;

#[cfg(feature = "cli")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let collector = review_engine::server::log_collector::init_global_collector();
    if std::env::var("REVIEW_LOG_FORMAT").as_deref() == Ok("json") {
        tracing_subscriber::fmt()
            .json()
            .with_current_span(false)
            .with_target(true)
            .with_writer(move || review_engine::server::log_collector::LogWriter::new(collector.clone()))
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_writer(move || review_engine::server::log_collector::LogWriter::new(collector.clone()))
            .init();
    }
    cli::run().await
}

#[cfg(not(feature = "cli"))]
fn main() {
    eprintln!("This binary requires the 'cli' feature");
    std::process::exit(1);
}
