//! Shared driver for cross-provider normalization scenarios.

mod assertions;
mod config;
mod scenarios;

/// Runs every scenario through each configured provider in a stable order.
pub(crate) async fn run_configured_provider_matrix() -> Result<(), String> {
    let targets = config::configured_targets()?;
    if targets.is_empty() {
        eprintln!(
            "skipping cross-provider normalization matrix: no endpoint credentials are configured"
        );
        return Ok(());
    }

    for target in &targets {
        scenarios::run_provider_suite(target).await?;
    }

    Ok(())
}
