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
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("healthcheck") {
        return healthcheck::run(&args[1..]).await;
    }
    if args.first().is_some_and(|s| s == "credential") {
        let receipt =
            calendar_server::credentials_bootstrap::run_credential_cli(args.into_iter().skip(1))
                .await?;
        println!("{}", serde_json::to_string(&receipt)?);
        return Ok(());
    }
    let config = DeploymentConfig::from_args(args)?;
    // Register the termination handler before migrations or binding to avoid a startup race.
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let server = calendar_server::production_runtime::initialize(config).await?;
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
