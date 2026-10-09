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
