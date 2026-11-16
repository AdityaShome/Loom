use loom_server::gateway;
use std::{net::SocketAddr, sync::Arc};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "loom_server=info".into()),
        )
        .init();

    let anthropic = std::env::var("LOOM_ANTHROPIC_UPSTREAM")
        .or_else(|_| std::env::var("LOOM_UPSTREAM"))
        .unwrap_or_else(|_| "https://api.anthropic.com".into());
    let openai =
        std::env::var("LOOM_OPENAI_UPSTREAM").unwrap_or_else(|_| "https://api.openai.com".into());
    let listen: SocketAddr = std::env::var("LOOM_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:7070".into())
        .parse()
        .expect("LOOM_LISTEN must be host:port");

    let app = gateway::router(Arc::new(gateway::State::new(anthropic.clone(), openai.clone())));
    let listener = tokio::net::TcpListener::bind(listen).await.expect("bind");
    tracing::info!(%listen, %anthropic, %openai, "loom listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve");
}
