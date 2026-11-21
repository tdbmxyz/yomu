use super::*;

fn config() -> FlareSolverrConfig {
    FlareSolverrConfig {
        url: "http://127.0.0.1:8191/".parse().unwrap(),
        timeout_secs: 60,
    }
}

#[test]
fn rejects_invalid_service_endpoints_and_timeouts() {
    for endpoint in [
        "ftp://localhost/",
        "https://user:pass@localhost/",
        "http://localhost/v1",
        "http://localhost/?secret=x",
        "http://localhost/#x",
    ] {
        let mut cfg = config();
        cfg.url = endpoint.parse().unwrap();
        assert!(FlareSolverr::new(cfg).is_err(), "{endpoint}");
    }
    for timeout in [0, 4, 121, u64::MAX] {
        let mut cfg = config();
        cfg.timeout_secs = timeout;
        assert!(FlareSolverr::new(cfg).is_err());
    }
}

fn scope() -> FlareSolverrSourceConfig {
    FlareSolverrSourceConfig {
        allowed_hosts: vec!["site.test".into(), "cdn.site.test".into()],
    }
}

#[test]
fn rejects_invalid_source_opt_in_lists() {
    let base = "https://site.test/".parse().unwrap();
    assert!(scope().validate(&base).is_ok());
    for hosts in [
        vec![],
        vec!["*.site.test".into()],
        vec!["site.test/path".into()],
        vec!["https://site.test".into()],
        vec!["other.test".into()],
    ] {
        assert!(
            FlareSolverrSourceConfig {
                allowed_hosts: hosts
            }
            .validate(&base)
            .is_err()
        );
    }
}

#[test]
fn clearance_cookies_respect_domain_path_secure_expiry_and_host_allowlist() {
    let cfg = scope();
    let solved: Url = "https://site.test/title".parse().unwrap();
    let mut cookie = Cookie {
        name: "cf_clearance".into(),
        value: "secret".into(),
        domain: ".site.test".into(),
        path: "/read".into(),
        secure: true,
        expires: now() + 3600.0,
    };
    assert!(cookie.safe(&solved, &cfg.allowed_hosts));
    assert!(cookie.usable(&"https://cdn.site.test/read/1".parse().unwrap()));
    for url in [
        "http://cdn.site.test/read/1",
        "https://cdn.site.test/reading",
        "https://site.test.evil/read/1",
        "https://other.test/read/1",
    ] {
        assert!(!cookie.usable(&url.parse().unwrap()), "{url}");
    }
    cookie.expires = now() - 1.0;
    assert!(!cookie.usable(&"https://cdn.site.test/read/1".parse().unwrap()));
    cookie.domain = ".test".into();
    assert!(!cookie.safe(&solved, &cfg.allowed_hosts));
    cookie.domain = ".site.test".into();
    cookie.value = "x\r\nInjected: yes".into();
    assert!(!cookie.safe(&solved, &cfg.allowed_hosts));
}

#[test]
fn credentials_never_leave_the_explicit_site_and_cdn_hosts() {
    let session = SolverSession::new(Arc::new(FlareSolverr::new(config()).unwrap()), scope());
    *session.state.write().unwrap() = Clearance {
        generation: 1,
        user_agent: Some(HeaderValue::from_static("Browser")),
        cookies: vec![Cookie {
            name: "clearance".into(),
            value: "secret".into(),
            domain: ".site.test".into(),
            path: "/".into(),
            secure: true,
            expires: -1.0,
        }],
        page_url: None,
    };
    assert_eq!(
        session.headers(&"https://cdn.site.test/page".parse().unwrap())[COOKIE],
        "clearance=secret"
    );
    for url in [
        "https://other.site.test/page",
        "https://site.test.evil/page",
        "https://other.test/page",
    ] {
        assert!(session.headers(&url.parse().unwrap()).is_empty());
    }
}
