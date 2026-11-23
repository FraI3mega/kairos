use std::str::FromStr;

use tracing_subscriber::filter::Targets;
use tracing_subscriber::prelude::*;

#[tokio::main]
async fn main() {
    let filter = std::env::var("KAIROS_LOG")
        .ok()
        .and_then(|v| Targets::from_str(&v).ok())
        .unwrap_or_else(|| Targets::new().with_default(tracing::Level::INFO));
    tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().with_ansi(false)).with(filter).init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match kairos_server::cli::parse(&args) {
        Ok(cmd) => kairos_server::cli::run(cmd).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        eprintln!("kairos-server: {e:#}");
        std::process::exit(1);
    }
}
