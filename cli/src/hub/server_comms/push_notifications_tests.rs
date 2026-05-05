/// Reproduces the panic from `set_notifications_enabled`:
/// reqwest::blocking::Client cannot `.send()` inside a tokio runtime
/// because it internally drops a runtime in an async context.
#[test]
#[should_panic(expected = "Cannot drop a runtime")]
pub(super) fn test_reqwest_blocking_inside_tokio_panics() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::blocking::Client::new();
    rt.block_on(async {
        // This is exactly what set_notifications_enabled did:
        // blocking HTTP inside the select! loop's block_on context.
        let _ = client
            .patch("http://127.0.0.1:1/hubs/1")
            .json(&serde_json::json!({"notifications_enabled": true}))
            .send();
    });
}

/// Proves that wrapping the blocking HTTP call in `block_in_place`
/// prevents the nested-runtime panic.
#[test]
pub(super) fn test_reqwest_blocking_with_block_in_place_works() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_millis(50))
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::task::block_in_place(|| {
            // Will fail to connect (no server), but won't panic
            let result = client
                .patch("http://127.0.0.1:1/hubs/1")
                .json(&serde_json::json!({"notifications_enabled": true}))
                .send();
            assert!(result.is_err()); // connection refused, not a panic
        });
    });
}
