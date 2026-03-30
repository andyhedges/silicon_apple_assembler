use arm64_sandbox::api;
use clap::Parser;
use tracing::info;

#[derive(Parser)]
#[command(name = "arm64-sandbox", version, about = "ARM64 Assembly Sandbox & Benchmarking API")]
struct Cli {
    /// Port to listen on
    #[arg(long, default_value_t = 80)]
    port: u16,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .with_timer(tracing_subscriber::fmt::time::UtcTime::rfc_3339())
        .init();

    info!(port = cli.port, "Starting ARM64 Sandbox API server");

    let app = api::create_router();

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", cli.port))
        .await
        .expect("Failed to bind to port");

    info!(port = cli.port, "Listening");

    axum::serve(listener, app)
        .await
        .expect("Server error");
}