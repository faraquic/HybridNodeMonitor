use crate::{
    config::{Application, Config},
    health_check::HealthCheckResult,
};
use log::{debug, error, info, warn};
use std::{
    sync::{Arc, RwLock},
    time::Duration,
};
use tokio::io::AsyncWriteExt;

/// HTTP(s)
pub async fn http(
    config: Arc<RwLock<Config>>,
    client: Arc<reqwest::Client>,
    app: Application,
) -> HealthCheckResult {
    info!("HTTP check: GET {} for '{}'", app.check.url, app.name());

    // [ ] TODO: ! FIX HTTPS SSL/TLS cert check
    let cert_warning = if app.check.url.starts_with("https://") {
        let tls_warn_days = { config.read().unwrap().tls_warn_days };

        tls_cert_warning(&app.check.url, tls_warn_days).await
    } else {
        None
    };

    match client.get(&app.check.url).send().await {
        Ok(response) => {
            let status = response.status();
            let expected = app.check.expect.status_code;
            let is_available = status == expected;

            debug!(
                "'{}': response status={}, expected={}",
                app.name(),
                status.as_u16(),
                expected
            );

            if is_available {
                info!(
                    "'{}': check passed (status={})",
                    app.name(),
                    status.as_u16()
                );
            } else {
                warn!(
                    "'{}': check failed — got status={}, expected={}",
                    app.name(),
                    status.as_u16(),
                    expected
                );
            }

            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available,
                status_code: Some(status.as_u16()),
                error: None,
                cert_warning,
            }
        }
        Err(e) => {
            error!("'{}': request to {} failed: {e}", app.name(), app.check.url);
            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available: false,
                status_code: None,
                error: Some(e.to_string()),
                cert_warning,
            }
        }
    }
}

/// TCP
pub async fn tcp(app: Application) -> HealthCheckResult {
    info!(
        "TCP check: connect to {} for '{}'",
        app.check.url,
        app.name()
    );

    // [ ] TODO: Tray auto-format
    // The URL must be specified in the format “host:port”
    // [ ] TODO: Move timeout to master conf
    let result = tokio::time::timeout(
        Duration::from_secs_f32(5f32),
        tokio::net::TcpStream::connect(&app.check.url),
    )
    .await;

    match result {
        Ok(Ok(mut stream)) => {
            debug!(
                "'{}': TCP connection established to {}",
                app.name(),
                app.check.url
            );

            let _ = stream.shutdown().await;
            info!("'{}': TCP check passed", app.name());
            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available: true,
                status_code: None,
                error: None,
                cert_warning: None,
            }
        }
        Ok(Err(e)) => {
            error!(
                "'{}': TCP connect to {} failed: {e}",
                app.name(),
                app.check.url
            );
            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available: false,
                status_code: None,
                error: Some(e.to_string()),
                cert_warning: None,
            }
        }
        Err(_) => {
            warn!(
                "'{}': TCP connect to {} timed out",
                app.name(),
                app.check.url
            );
            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available: false,
                status_code: None,
                error: Some("connection timed out".into()),
                cert_warning: None,
            }
        }
    }
}

/// Exec
/// app.check.url is interpreted as a command followed by arguments separated by spaces.
/// Success = exit code 0 (or a match with expect.exit_code if specified).
// [ ] TODO: Add/Check env vars
// [ ] TODO: Check output
pub async fn exec(app: Application) -> HealthCheckResult {
    info!("Exec check: run '{}' for '{}'", app.check.url, app.name());

    let mut parts = app.check.url.split_whitespace();
    let cmd = match parts.next() {
        Some(c) => c,
        None => {
            error!("'{}': exec command is empty", app.name());
            return HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available: false,
                status_code: None,
                error: Some("empty command".into()),
                cert_warning: None,
            };
        }
    };
    let args: Vec<&str> = parts.collect();

    debug!("'{}': executing '{cmd}' with args {args:?}", app.name());

    let result = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(cmd).args(&args).output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let expected_code = app.check.expect.exit_code;
            let is_available = exit_code == expected_code;

            debug!(
                "'{}': exit code={exit_code}, expected={expected_code}",
                app.name()
            );

            if !output.stdout.is_empty() {
                debug!(
                    "'{}': stdout={}",
                    app.name(),
                    String::from_utf8_lossy(&output.stdout).trim()
                );
            }
            if !output.stderr.is_empty() {
                debug!(
                    "'{}': stderr={}",
                    app.name(),
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }

            if is_available {
                info!("'{}': exec check passed (exit={exit_code})", app.name());
            } else {
                warn!(
                    "'{}': exec check failed — exit={exit_code}, expected={expected_code}",
                    app.name()
                );
            }

            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available,
                status_code: Some(exit_code as u16),
                error: if is_available {
                    None
                } else {
                    Some(format!("exit code {exit_code}"))
                },
                cert_warning: None,
            }
        }
        Ok(Err(e)) => {
            error!("'{}': failed to spawn process '{cmd}': {e}", app.name());
            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available: false,
                status_code: None,
                error: Some(format!("spawn error: {e}")),
                cert_warning: None,
            }
        }
        Err(_) => {
            warn!("'{}': exec command '{cmd}' timed out", app.name());
            HealthCheckResult {
                id: app.id.clone(),
                app_name: app.name(),
                is_available: false,
                status_code: None,
                error: Some("command timed out".into()),
                cert_warning: None,
            }
        }
    }
}

/// Connects via TLS and checks the certificate's validity period.
/// Returns Some (a warning) if fewer than warn_days days remain.
async fn tls_cert_warning(url: &str, warn_days: i64) -> Option<String> {
    use rustls::pki_types::ServerName;
    use rustls_platform_verifier::BuilderVerifierExt;
    use tokio_rustls::TlsConnector;
    use x509_parser::prelude::{FromDer, X509Certificate};

    let host = url
        .trim_start_matches("https://")
        .split('/')
        .next()?
        .split(':')
        .next()?;
    let port = url
        .trim_start_matches("https://")
        .split(':')
        .nth(1)
        .and_then(|p| p.split('/').next())
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(443);

    debug!("Checking TLS cert for {host}:{port} (warn_days={warn_days})");

    let config = Arc::new(
        rustls::ClientConfig::builder()
            .with_platform_verifier()
            .unwrap()
            .with_no_client_auth(),
    );
    let connector = TlsConnector::from(config);

    let stream = match tokio::net::TcpStream::connect((host, port)).await {
        Ok(s) => s,
        Err(e) => {
            warn!("TLS cert check: TCP connect to {host}:{port} failed: {e}");
            return None;
        }
    };

    let server_name = match ServerName::try_from(host.to_string()) {
        Ok(n) => n,
        Err(e) => {
            warn!("TLS cert check: invalid server name '{host}': {e}");
            return None;
        }
    };

    let tls_stream = match connector.connect(server_name, stream).await {
        Ok(s) => s,
        Err(e) => {
            warn!("TLS cert check: handshake with {host}:{port} failed: {e}");
            return None;
        }
    };

    let (_, session) = tls_stream.get_ref();
    let certs = session.peer_certificates()?;
    let raw = certs.first()?.as_ref();

    let (_, cert) = match X509Certificate::from_der(raw) {
        Ok(c) => c,
        Err(e) => {
            warn!("TLS cert check: failed to parse certificate for {host}: {e}");
            return None;
        }
    };

    let not_after = cert.validity().not_after.timestamp();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    let days_left = (not_after - now) / 86_400;
    debug!("'{}' TLS cert expires in {days_left} day(s)", host);

    if days_left <= 0 {
        Some(format!("Certificate EXPIRED {}", -days_left))
    } else if days_left <= warn_days {
        Some(format!("Certificate expires in {days_left} day(s)"))
    } else {
        None
    }
}
