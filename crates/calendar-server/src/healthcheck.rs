//! Local probe only: never initializes storage, starts workers or follows remote redirects.
use calendar_server::Config;
use std::time::Duration;

pub async fn run(arguments: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (mode, deployment, timeout) = options(arguments)?;
    let mut address = Config::deployment_bind_from_args(deployment.iter().cloned())?;
    // Wildcard listeners are reached through the corresponding loopback family.
    if address.ip().is_unspecified() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    if !address.ip().is_loopback() || address.port() == 0 {
        return Err("healthcheck requires a nonzero loopback port".into());
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|_| "healthcheck client initialization failed")?;
    let response = client
        .get(format!("http://{address}/health/{mode}"))
        .send()
        .await
        .map_err(|_| "healthcheck request failed")?;
    if response.status() != reqwest::StatusCode::OK {
        return Err("healthcheck endpoint is unhealthy".into());
    }
    Ok(())
}

fn options(arguments: &[String]) -> Result<(&str, &[String], Duration), &'static str> {
    let usage =
        "usage: calendar-server healthcheck [live|ready] --deployment-config FILE [--timeout-ms N]";
    let (mode, rest) = match arguments.first().map(String::as_str) {
        Some("live" | "ready") => (arguments[0].as_str(), &arguments[1..]),
        _ => ("live", arguments),
    };
    let (deployment, timeout) = match rest {
        [flag, _] if flag == "--deployment-config" => (rest, 2000),
        [flag, _, timeout_flag, value]
            if flag == "--deployment-config" && timeout_flag == "--timeout-ms" =>
        {
            (
                &rest[..2],
                value
                    .parse::<u64>()
                    .map_err(|_| "invalid healthcheck timeout")?,
            )
        }
        _ => return Err(usage),
    };
    if !(1..=120_000).contains(&timeout) {
        return Err("invalid healthcheck timeout");
    }
    Ok((mode, deployment, Duration::from_millis(timeout)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| (*s).to_owned()).collect()
    }
    #[test]
    fn health_arguments_keep_one_deployment_input_and_a_bounded_health_timeout() {
        for mode in ["live", "ready"] {
            let input = args(&[
                mode,
                "--deployment-config",
                "file.json",
                "--timeout-ms",
                "100",
            ]);
            let (parsed, deployment, timeout) = options(&input).unwrap();
            assert_eq!(parsed, mode);
            assert_eq!(deployment, &input[1..3]);
            assert_eq!(timeout, Duration::from_millis(100));
        }
        let input = args(&["--deployment-config", "file.json"]);
        assert_eq!(options(&input).unwrap().2, Duration::from_secs(2));
        for input in [
            args(&[]),
            args(&["unknown", "--deployment-config", "file"]),
            args(&["--deployment-config", "a", "--deployment-config", "b"]),
            args(&["--deployment-config", "file", "--timeout-ms", "0"]),
            args(&["--deployment-config", "file", "--timeout-ms", "120001"]),
            args(&[
                "--deployment-config",
                "file",
                "--timeout-ms",
                "secret-invalid",
            ]),
        ] {
            let error = options(&input).unwrap_err();
            assert!(!error.contains("secret-invalid"));
        }
    }
}
