//! R-23 tests: iframe drilling + SPA dynamic element interaction.
//!
//! Unit tests (non-ignored) verify the MockBackend contract.
//! Live-browser tests are `#[ignore]` and require `--features live-browser`.

use oxide_browser_sh::{BrowserBackend, BrowserError, MockBackend};

// ---------------------------------------------------------------------------
// Unit tests — run always (no live browser needed)
// ---------------------------------------------------------------------------

/// MockBackend must return Unsupported for frame() — documented contract.
#[tokio::test]
async fn mock_backend_frame_returns_unsupported() {
    let backend = MockBackend::new();
    let err = backend
        .frame("inner")
        .await
        .err()
        .expect("frame() should fail on MockBackend");
    assert!(
        matches!(err, BrowserError::Unsupported(_)),
        "expected Unsupported, got {err}"
    );
}

// ---------------------------------------------------------------------------
// Live-browser tests — require Chromium + live-browser feature
// ---------------------------------------------------------------------------

/// Drill into a nested iframe and click a button inside it.
#[ignore]
#[cfg(feature = "live-browser")]
#[tokio::test]
async fn chromium_frame_drilling_nested_iframes() {
    use oxide_browser_sh::{chromium::ChromiumBackend, Selector};
    use std::path::PathBuf;

    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nested_iframes.html");
    let url = format!("file://{}", fixture.display());

    let (backend, _handle) = ChromiumBackend::launch_headless().await.unwrap();
    backend.navigate(&url).await.unwrap();

    // Drill into the outer frame, then the inner frame.
    let outer = backend.frame("outer").await.unwrap();
    let inner = outer.frame("inner").await.unwrap();

    // Click a button inside the innermost iframe.
    inner
        .click(&Selector::Css("#inner-btn".into()))
        .await
        .unwrap();
}

/// Navigate to a React-style SPA fixture, wait for dynamic button, click it.
#[ignore]
#[cfg(feature = "live-browser")]
#[tokio::test]
async fn chromium_spa_dynamic_button_click() {
    use oxide_browser_sh::{chromium::ChromiumBackend, Selector};
    use std::path::PathBuf;

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spa.html");
    let url = format!("file://{}", fixture.display());

    let (backend, _handle) = ChromiumBackend::launch_headless().await.unwrap();
    backend.navigate(&url).await.unwrap();

    // Wait up to 2s for the dynamic button to appear.
    let sel = Selector::Role {
        role: "button".into(),
        name: "Dynamic Button".into(),
    };
    let mut found = false;
    for _ in 0..20 {
        if backend.click(&sel).await.is_ok() {
            found = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(found, "dynamic button never appeared or click failed");
}
