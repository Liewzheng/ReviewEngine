use super::{bin_path, find_free_port, spawn_server_full, wait_for_server};

// ─── Static frontend Cache-Control (SPA white-screen defect) ─────

/// Write a minimal bundler-style dist tree (`index.html` + content-hashed
/// asset + favicon) under `{root}/frontend/dist`. The server resolves
/// `./frontend/dist` relative to its working directory, so the test points
/// the spawned server's CWD at `root` — independent of a real frontend build
/// (frontend/dist is gitignored and absent in CI).
fn write_fake_frontend_dist(root: &std::path::Path) {
    let dist = root.join("frontend").join("dist");
    let assets = dist.join("assets");
    std::fs::create_dir_all(&assets).expect("failed to create fixture assets dir");
    std::fs::write(
        dist.join("index.html"),
        "<!doctype html><html><body><div id=\"app\">fixture</div></body></html>\n",
    )
    .expect("failed to write fixture index.html");
    std::fs::write(assets.join("app-CqstUsos.js"), "console.log('fixture');\n").expect("failed to write fixture asset");
    std::fs::write(
        dist.join("favicon.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>\n",
    )
    .expect("failed to write fixture favicon");
}

/// Defect regression: the SPA's assets are content-hashed, so `index.html`
/// must be revalidated on every load — a stale cached copy references chunks
/// that no longer exist after an upgrade and leaves users on a blank page —
/// while the hashed assets themselves are safe to cache immutably.
#[tokio::test]
async fn static_frontend_cache_control_headers() {
    let www = tempfile::tempdir().expect("failed to create www temp dir");
    write_fake_frontend_dist(www.path());

    let port = find_free_port();
    let _guard = spawn_server_full(&bin_path(), port, None, &[], Some(www.path()));
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");
    let cache_control = |resp: &reqwest::Response| {
        resp.headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };

    // `/` serves index.html and must be revalidated on every load.
    let resp = client.get(format!("{base}/")).send().await.expect("GET /");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "GET / returned {}",
        resp.status()
    );
    let cc = cache_control(&resp).unwrap_or_default();
    assert!(cc.contains("no-cache"), "GET / must be no-cache, got {cc:?}");
    assert!(
        cc.contains("must-revalidate"),
        "GET / must require revalidation, got {cc:?}"
    );
    let body = resp.text().await.expect("GET / body");
    assert!(
        body.contains("fixture"),
        "GET / must serve the fixture index.html, got {body:?}"
    );

    // Direct `/index.html` — same policy.
    let resp = client
        .get(format!("{base}/index.html"))
        .send()
        .await
        .expect("GET /index.html");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let cc = cache_control(&resp).unwrap_or_default();
    assert!(cc.contains("no-cache"), "GET /index.html must be no-cache, got {cc:?}");

    // Content-hashed asset — cache for a year, never revalidate.
    let resp = client
        .get(format!("{base}/assets/app-CqstUsos.js"))
        .send()
        .await
        .expect("GET hashed asset");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::OK,
        "GET asset returned {}",
        resp.status()
    );
    let cc = cache_control(&resp).unwrap_or_default();
    assert!(cc.contains("immutable"), "hashed asset must be immutable, got {cc:?}");
    assert!(
        cc.contains("max-age=31536000"),
        "hashed asset must cache for a year, got {cc:?}"
    );

    // API/health routes keep the status quo: no Cache-Control header at all.
    let resp = client.get(format!("{base}/health")).send().await.expect("GET /health");
    assert!(resp.status().is_success());
    assert!(
        cache_control(&resp).is_none(),
        "/health must not gain a Cache-Control header, got {:?}",
        cache_control(&resp)
    );

    // Non-hashed static files keep browser defaults.
    let resp = client
        .get(format!("{base}/favicon.svg"))
        .send()
        .await
        .expect("GET /favicon.svg");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(
        cache_control(&resp).is_none(),
        "favicon.svg must not gain a Cache-Control header, got {:?}",
        cache_control(&resp)
    );

    // A missing hashed asset must NOT be cached immutably — an immutably
    // cached 404 would keep the app broken in browsers even after the file is
    // deployed.
    let resp = client
        .get(format!("{base}/assets/missing-00000000.js"))
        .send()
        .await
        .expect("GET missing asset");
    assert_eq!(resp.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(
        cache_control(&resp).is_none(),
        "404 for a missing asset must not be cached, got {:?}",
        cache_control(&resp)
    );

    // Conditional request: revalidation must answer 304 AND carry the same
    // Cache-Control as the 200 (RFC 9110 §15.4.5). This pins the contract
    // that makes `no-cache` cheap — every load revalidates, unchanged deploys
    // cost a 304 instead of a full download. If a future tower-http upgrade
    // or a handler swap silently drops 304 support or the header, this fails.
    let resp = client
        .get(format!("{base}/"))
        .send()
        .await
        .expect("GET / for Last-Modified");
    let last_modified = resp
        .headers()
        .get("last-modified")
        .and_then(|v| v.to_str().ok())
        .expect("GET / must carry a Last-Modified header")
        .to_owned();
    let resp = client
        .get(format!("{base}/"))
        .header("if-modified-since", &last_modified)
        .send()
        .await
        .expect("GET / with If-Modified-Since");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_MODIFIED,
        "conditional GET must revalidate to 304, got {}",
        resp.status()
    );
    let cc = cache_control(&resp).unwrap_or_default();
    assert!(
        cc.contains("no-cache"),
        "304 must carry the same Cache-Control as the 200, got {cc:?}"
    );
}

// ─── SPA history-mode fallback (deep-link 404 defect) ────────────

/// Defect regression: the SPA uses history-mode routing, so directly opening
/// or refreshing a client-side route (`/history`, `/config`, …) must serve
/// `index.html` — ServeDir's bare 404 left deep links dead. Unmatched `/api/`
/// routes and missing files (extension in the last segment) must keep their
/// 404: serving HTML for a missing hashed asset would mask deploy breakage.
#[tokio::test]
async fn spa_deep_links_fall_back_to_index_html() {
    let www = tempfile::tempdir().expect("failed to create www temp dir");
    write_fake_frontend_dist(www.path());

    let port = find_free_port();
    let _guard = spawn_server_full(&bin_path(), port, None, &[], Some(www.path()));
    wait_for_server(port).await;

    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}");

    // Deep links — single segment and nested — serve the entry point with the
    // same revalidation policy as `/`.
    for path in ["/history", "/config", "/reviews/42"] {
        let resp = client
            .get(format!("{base}{path}"))
            .send()
            .await
            .unwrap_or_else(|e| panic!("GET {path}: {e}"));
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "GET {path} must serve index.html, got {}",
            resp.status()
        );
        let cc = resp
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_default();
        assert!(cc.contains("no-cache"), "GET {path} must be no-cache, got {cc:?}");
        let body = resp.text().await.expect("body");
        assert!(
            body.contains("fixture"),
            "GET {path} must serve the fixture index.html, got {body:?}"
        );
    }

    // An unmatched API route stays a 404 — it must never be answered with the
    // SPA entry point.
    let resp = client
        .get(format!("{base}/api/v1/definitely-not-a-route"))
        .send()
        .await
        .expect("GET unknown api route");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "unknown /api/ route must stay 404, got {}",
        resp.status()
    );

    // A missing file (hashed asset) stays a 404 — an HTML 200 here would hide
    // a broken deploy behind a white screen.
    let resp = client
        .get(format!("{base}/assets/missing-00000000.js"))
        .send()
        .await
        .expect("GET missing asset");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "missing asset must stay 404, got {}",
        resp.status()
    );
}
