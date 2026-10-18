//! `web_fetch` is a read-only tool, so it is auto-approved in every permission
//! mode and the model picks the URL. That makes it an SSRF primitive unless the
//! target is checked: without a guard, a prompt-injected page or repository file
//! can point it at the cloud metadata service, a router, or a service on the
//! user's own machine, and read the response back with no prompt at all.

use super::tools::is_private_target;

#[test]
fn the_public_internet_is_allowed() {
    for url in [
        "https://example.com",
        "https://docs.rs/tokio/latest/tokio/",
        "http://93.184.216.34/",           // a public v4 literal
        "https://[2606:4700:4700::1111]/", // a public v6 literal
        "https://user:pw@example.com/x",   // userinfo must not confuse the parse
        "https://example.com:8443/x",      // a port must not either
    ] {
        assert!(
            !is_private_target(url),
            "{url} is public and must be allowed"
        );
    }
}

#[test]
fn loopback_and_metadata_are_refused() {
    for url in [
        "http://localhost:3000/admin",
        "http://127.0.0.1:8080/",
        "http://127.1.2.3/", // the whole 127/8, not just 127.0.0.1
        "https://[::1]:9000/",
        "http://[::ffff:127.0.0.1]/",               // v4-mapped loopback
        "http://169.254.169.254/latest/meta-data/", // AWS/GCP/Azure metadata
        "http://metadata.google.internal/",
        "http://LOCALHOST/", // case must not matter
        "http://foo.localhost/",
    ] {
        assert!(is_private_target(url), "{url} is local and must be refused");
    }
}

#[test]
fn private_ranges_and_intranet_names_are_refused() {
    for url in [
        "http://10.0.0.5/",
        "http://172.16.3.4/",  // RFC1918
        "http://192.168.1.1/", // the router, for most people
        "http://[fe80::1]/",   // link-local v6
        "http://[fd00::1]/",   // unique-local v6
        "http://100.64.0.1/",  // CGNAT
        "http://0.0.0.0/",
        "http://intranet/", // a bare name: no public DNS entry
        "http://wiki:8080/",
    ] {
        assert!(
            is_private_target(url),
            "{url} is private and must be refused"
        );
    }
}

#[test]
fn a_non_http_scheme_is_not_our_problem_to_judge() {
    // `web_fetch` rejects these on the scheme check before this function is
    // reached; the guard must not start refusing things it never sees.
    for url in ["ftp://example.com", "file:///c:/windows", "example.com"] {
        assert!(
            !is_private_target(url),
            "{url} is rejected earlier, not here"
        );
    }
}

// ---- Redirects ----------------------------------------------------------
//
// The pre-check above only sees the URL the model wrote. A public page that
// answers with a 30x into a private range must be refused hop by hop, or the
// pre-check is decoration. These tests stand up a tiny HTTP server on
// 127.0.0.1 and reach it under a dotted test hostname (pinned with
// `resolve`), so the starting URL passes the pre-check exactly as a real
// public URL would. No live network is involved.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PUBLIC_NAME: &str = "redirector.example.com";

/// Serve until the test ends. `/to-private` 302s to the loopback literal,
/// `/to-public` 302s to `/final` under the public name, `/loop` redirects to
/// itself forever, and `/secret` counts every hit so a test can prove it was
/// never reached.
async fn redirect_server() -> (u16, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let secret_hits = Arc::new(AtomicUsize::new(0));
    let loop_hits = Arc::new(AtomicUsize::new(0));
    let (secret, looped) = (secret_hits.clone(), loop_hits.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (secret, looped) = (secret.clone(), looped.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let redirect = |to: String| {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                };
                let ok = |body: &str| {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let resp = match path.as_str() {
                    "/to-private" => redirect(format!("http://127.0.0.1:{port}/secret")),
                    "/to-dotted-localhost" => redirect(format!("http://localhost.:{port}/secret")),
                    "/to-metadata" => redirect("http://169.254.169.254/latest/meta-data/".into()),
                    "/to-public" => redirect(format!("http://{PUBLIC_NAME}:{port}/final")),
                    "/loop" => {
                        looped.fetch_add(1, Ordering::SeqCst);
                        redirect(format!("http://{PUBLIC_NAME}:{port}/loop"))
                    }
                    "/secret" => {
                        secret.fetch_add(1, Ordering::SeqCst);
                        ok("INTERNAL-SECRET")
                    }
                    "/final" => ok("public final page"),
                    _ => ok("hello"),
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (port, secret_hits, loop_hits)
}

/// The production web-fetch client, with the test hostname pinned to the
/// local server and system proxies off so the request really goes there.
fn test_client(port: u16) -> reqwest::Client {
    super::tools::web_client_builder()
        .resolve(
            PUBLIC_NAME,
            std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        )
        .no_proxy()
        .build()
        .unwrap()
}

#[tokio::test]
async fn a_redirect_into_loopback_is_refused_and_never_fetched() {
    let (port, secret_hits, _) = redirect_server().await;
    let url = format!("http://{PUBLIC_NAME}:{port}/to-private");
    assert!(
        !is_private_target(&url),
        "the start URL must pass the pre-check"
    );
    let out = super::tools::web_fetch_with(&test_client(port), &url, 0).await;
    assert_eq!(
        secret_hits.load(Ordering::SeqCst),
        0,
        "the private redirect target was requested: {out:?}"
    );
    let err = out.expect_err("a redirect into loopback must fail the fetch");
    assert!(!err.contains("INTERNAL-SECRET"), "{err}");
    assert!(
        err.contains("redirect") && err.contains("private"),
        "the model should be told the redirect target was private: {err}"
    );
}

#[tokio::test]
async fn a_redirect_into_cloud_metadata_is_refused() {
    let (port, _, _) = redirect_server().await;
    let url = format!("http://{PUBLIC_NAME}:{port}/to-metadata");
    let err = super::tools::web_fetch_with(&test_client(port), &url, 0)
        .await
        .expect_err("a redirect to the metadata address must fail");
    assert!(err.contains("169.254.169.254"), "{err}");
    assert!(err.contains("private"), "{err}");
}

#[tokio::test]
async fn a_public_to_public_redirect_is_still_followed() {
    let (port, _, _) = redirect_server().await;
    let url = format!("http://{PUBLIC_NAME}:{port}/to-public");
    let out = super::tools::web_fetch_with(&test_client(port), &url, 0)
        .await
        .expect("an ordinary redirect must keep working");
    assert!(out.starts_with("HTTP 200"), "{out}");
    assert!(out.contains("public final page"), "{out}");
}

#[tokio::test]
async fn a_redirect_loop_stops_after_a_few_hops() {
    let (port, _, loop_hits) = redirect_server().await;
    let url = format!("http://{PUBLIC_NAME}:{port}/loop");
    let err = super::tools::web_fetch_with(&test_client(port), &url, 0)
        .await
        .expect_err("an endless redirect must fail");
    assert!(err.contains("redirect"), "{err}");
    let hits = loop_hits.load(Ordering::SeqCst);
    assert!(hits <= 6, "followed {hits} hops; the cap is 5 redirects");
}

#[test]
fn dotted_localhost_redirect_policy_is_refused() {
    let url = reqwest::Url::parse("http://localhost.:8080/secret").unwrap();
    assert_eq!(url.host_str(), Some("localhost."));
    assert!(
        !super::tools::redirect_allowed(&url),
        "a DNS root dot must not disguise localhost"
    );
}

#[test]
fn canonical_private_hosts_are_refused_before_fetch_and_at_each_hop() {
    for (raw, canonical_host) in [
        ("http://127.1/", "127.0.0.1"),
        ("http://0177.0.0.1/", "127.0.0.1"),
        ("http://0x7f.0x0.0x0.0x1/", "127.0.0.1"),
        ("http://2130706433/", "127.0.0.1"),
        ("http://%6cocalhost./", "localhost."),
        (
            "http://metadata.google.internal./",
            "metadata.google.internal.",
        ),
        ("http://[::ffff:127.0.0.1]:8080/", "[::ffff:7f00:1]"),
        ("http://[::1]:8080/", "[::1]"),
    ] {
        let url = reqwest::Url::parse(raw).unwrap();
        assert_eq!(url.host_str(), Some(canonical_host), "{raw}");
        assert!(is_private_target(raw), "initial URL must be refused: {raw}");
        assert!(
            !super::tools::redirect_allowed(&url),
            "redirect must be refused: {raw} -> {url}"
        );
    }
    for raw in [
        "http://LOCALHOST./",
        "http://sub.localhost./",
        "http://localhost.localdomain./",
        "http://intranet./",
    ] {
        assert!(is_private_target(raw), "{raw}");
        assert!(
            !super::tools::redirect_allowed(&reqwest::Url::parse(raw).unwrap()),
            "{raw}"
        );
    }
    for raw in ["https://example.com./", "http://93.184.216.34./"] {
        assert!(
            !is_private_target(raw),
            "public host must stay allowed: {raw}"
        );
        assert!(super::tools::redirect_allowed(
            &reqwest::Url::parse(raw).unwrap()
        ));
    }
}

#[tokio::test]
async fn a_redirect_into_dotted_localhost_is_refused_without_secret_hits() {
    let (port, secret_hits, _) = redirect_server().await;
    let url = format!("http://{PUBLIC_NAME}:{port}/to-dotted-localhost");
    assert!(!is_private_target(&url), "the initial URL is public");
    // Pin only the reserved localhost spelling to keep the test deterministic
    // and offline even on systems without localhost in their resolver config.
    let client = super::tools::web_client_builder()
        .resolve(PUBLIC_NAME, ([127, 0, 0, 1], port).into())
        .resolve("localhost.", ([127, 0, 0, 1], port).into())
        .no_proxy()
        .build()
        .unwrap();
    let result = super::tools::web_fetch_with(&client, &url, 0).await;
    assert_eq!(
        secret_hits.load(Ordering::SeqCst),
        0,
        "dotted localhost was fetched: {result:?}"
    );
    let err = result.expect_err("the redirect must be refused");
    assert!(err.contains("redirect") && err.contains("private"), "{err}");
    assert!(err.contains("localhost."), "{err}");
}

#[test]
fn the_redirect_hop_check_refuses_what_the_pre_check_refuses() {
    use super::tools::redirect_allowed;
    let url = |s: &str| reqwest::Url::parse(s).unwrap();
    for to in [
        "http://127.0.0.1:8080/secret",
        "http://localhost/",
        "http://169.254.169.254/latest/meta-data/",
        "http://metadata.google.internal/",
        "http://10.0.0.1/",
        "http://192.168.1.1/",
        "http://[::1]/",
        "http://[::ffff:127.0.0.1]/",
        "http://intranet/",
        "ftp://example.com/", // not a web hop at all
    ] {
        assert!(!redirect_allowed(&url(to)), "{to} must not be followed");
    }
    for to in ["https://example.com/next", "http://93.184.216.34/"] {
        assert!(redirect_allowed(&url(to)), "{to} must be followed");
    }
}
