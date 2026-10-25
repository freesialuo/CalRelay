//! Local probe only: never initializes storage, starts workers or follows remote redirects.
use std::{net::SocketAddr, time::Duration};

pub async fn run(arguments: &[String]) -> Result<(), &'static str> {
    let mode = match arguments {
        [] => "live",
        [mode] if mode == "live" || mode == "ready" => mode,
        _ => return Err("usage: calendar-server healthcheck [live|ready]"),
    };
    let mut address: SocketAddr = std::env::var("CALRELAY_BIND")
        .map_err(|_| "healthcheck requires CALRELAY_BIND")?
        .parse()
        .map_err(|_| "invalid healthcheck address")?;
    // Wildcard listeners are reached through the corresponding loopback family.
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("healthcheck requires a nonzero loopback port");
    }
    let timeout_ms: u64 = match std::env::var("CALRELAY_HEALTHCHECK_TIMEOUT_MS") {
        Ok(value) => value.parse().map_err(|_| "invalid healthcheck timeout")?,
        Err(std::env::VarError::NotPresent) => 2000,
        Err(_) => return Err("invalid healthcheck timeout"),
    };
    if !(1..=120_000).contains(&timeout_ms) {
        return Err("invalid healthcheck timeout");
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .map_err(|_| "healthcheck client initialization failed")?;
    let response = client
        .get(format!("http://{address}/health/{mode}"))
        .send()
        .await
        .map_err(|_| "healthcheck request failed")?;
    if response.status() != reqwest::StatusCode::OK {
        return Err("healthcheck endpoint is unhealthy");
    }
    Ok(())
}
