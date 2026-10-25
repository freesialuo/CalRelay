use calendar_server::{Config, Server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;
    // Register the termination handler before migrations or binding to avoid a startup race.
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let server = Server::initialize(config).await?;
    let listener = server.bind().await?;
    eprintln!("CalRelay listening on {}", listener.local_addr()?);
    server
        .serve(listener, async move {
            #[cfg(unix)]
            tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
