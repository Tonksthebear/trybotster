//! Hosted preview DNS readiness gate.
//!
//! cloudflared prints the quick-tunnel URL before DNS has globally propagated.
//! If the browser navigates immediately, the system resolver caches the
//! NXDOMAIN for ~30 minutes (SOA minimum TTL).  We gate on DNS resolution via
//! Cloudflare DoH (1.1.1.1) — authoritative for trycloudflare.com, so the
//! answer is fresh every query — and only surface the URL once it resolves.

/// Poll Cloudflare DoH until the hostname resolves or the timeout expires.
///
/// Returns `Ok(())` as soon as DNS returns at least one A record.
/// No HTTPS probe — the tunnel is already registered by the time cloudflared
/// prints the URL; we only need to wait for DNS propagation.
pub async fn wait_until_dns_ready(hostname: &str, timeout: std::time::Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .map_err(|e| format!("failed to build DoH client: {e}"))?;

    let doh_url = format!("https://1.1.1.1/dns-query?name={hostname}&type=A");
    let mut last_error = "dns never resolved".to_string();

    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(last_error);
        }

        match client
            .get(&doh_url)
            .header("Accept", "application/dns-json")
            .send()
            .await
        {
            Ok(resp) => match resp.json::<serde_json::Value>().await {
                Ok(json) => {
                    let status = json.get("Status").and_then(|v| v.as_u64()).unwrap_or(99);
                    if status == 0 {
                        // NOERROR — check for at least one A record
                        let has_a = json
                            .get("Answer")
                            .and_then(|v| v.as_array())
                            .map_or(false, |answers| {
                                answers.iter().any(|a| {
                                    a.get("type").and_then(|v| v.as_u64()) == Some(1)
                                })
                            });
                        if has_a {
                            return Ok(());
                        }
                        last_error = "dns returned NOERROR but no A records".to_string();
                    } else {
                        last_error = format!("dns returned rcode {status}");
                    }
                }
                Err(e) => last_error = format!("DoH parse failed: {e}"),
            },
            Err(e) => last_error = format!("DoH request failed: {e}"),
        }

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::wait_until_dns_ready;

    #[tokio::test]
    async fn rejects_bogus_hostname_quickly() {
        // Should fail fast — no DNS record will ever exist for this
        let result = wait_until_dns_ready(
            "this-hostname-will-never-exist.trycloudflare.com",
            std::time::Duration::from_secs(2),
        )
        .await;
        assert!(result.is_err());
    }
}
