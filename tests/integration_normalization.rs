//! Opt-in cross-provider acceptance coverage for the normalized client API.

mod normalization;

use std::time::Duration;
use tokio::time::timeout;

/// Runs the same complete conversation matrix against every configured client.
///
/// The outer deadline keeps this entire test case below one minute even when
/// an endpoint stalls while executing one of the multi-request scenarios.
#[tokio::test]
#[ignore = "requires credentials for the Anthropic, OpenAI Responses, and/or OpenAI Chat/Completions real endpoint"]
async fn configured_providers_share_the_normalized_conversation_contract() {
    timeout(
        Duration::from_secs(55),
        normalization::run_configured_provider_matrix(),
    )
    .await
    .expect("cross-provider normalization matrix exceeded 55 seconds")
    .expect("cross-provider normalization matrix failed");
}
