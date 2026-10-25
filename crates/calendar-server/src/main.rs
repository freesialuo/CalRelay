mod healthcheck;
use calendar_server::DeploymentConfig;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.first().map(String::as_str) == Some("healthcheck") {
        return healthcheck::run(&arguments[1..]).await;
    }
    let config = DeploymentConfig::from_args(arguments)?;
    // Register the termination handler before migrations or binding to avoid a startup race.
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let server = config.initialize().await?;
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
