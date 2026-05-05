/// Proves that nesting `block_on` inside `block_on` panics.
///
/// This is the exact pattern that caused the WebRTC connection panic
/// before the `block_in_place` fix was applied to all 9 call sites
/// in this file.
#[test]
#[should_panic(expected = "Cannot start a runtime from within a runtime")]
pub(super) fn test_nested_block_on_panics_without_block_in_place() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        rt.block_on(async { 42 });
    });
}

/// Proves that `block_in_place` wrapping `block_on` prevents the
/// nested-runtime panic. This is the pattern used by all async
/// bridge points in this file.
#[test]
pub(super) fn test_block_in_place_prevents_nested_runtime_panic() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let result = tokio::task::block_in_place(|| rt.block_on(async { 42 }));
        assert_eq!(result, 42);
    });
}
