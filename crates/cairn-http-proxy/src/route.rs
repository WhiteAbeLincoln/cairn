use http::Request;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub methods: Vec<String>,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
}

impl Route {
    pub fn matches<B>(&self, request: &Request<B>) -> bool {
        let method_matches = self.methods.is_empty()
            || self
                .methods
                .iter()
                .any(|method| method.eq_ignore_ascii_case(request.method().as_str()));
        let host_matches = self.host.as_ref().is_none_or(|expected| {
            request_host(request)
                .is_some_and(|actual| normalize_host(&actual) == normalize_host(expected))
        });
        let path_matches = self
            .path_prefix
            .as_ref()
            .is_none_or(|prefix| request.uri().path().starts_with(prefix));
        method_matches && host_matches && path_matches
    }
}

fn request_host<B>(request: &Request<B>) -> Option<String> {
    request.uri().host().map(str::to_owned).or_else(|| {
        request
            .headers()
            .get(http::header::HOST)
            .and_then(|value| value.to_str().ok())
            // Parse as an `Authority` rather than naively splitting on ':' —
            // a bracketed IPv6 literal (`[::1]:8080`) contains colons inside
            // the host itself, so a plain `split(':').next()` would yield the
            // bogus host "[". `Authority::host()` strips the port/userinfo
            // correctly and preserves the IPv6 brackets.
            .and_then(|value| value.parse::<http::uri::Authority>().ok())
            .map(|authority| authority.host().to_owned())
    })
}

/// Normalizes a host for comparison so that equivalent representations of
/// the same host compare equal:
/// - a bracketed IPv6 literal (`[::1]`) is compared against its canonical,
///   unbracketed form, so shorthand and expanded forms of the same address
///   match (`[::1]` == `0:0:0:0:0:0:0:1`);
/// - non-IP hostnames are ASCII-lowercased and a single trailing "." (the
///   absolute/FQDN form) is stripped, so `Example.com.` == `example.com`.
fn normalize_host(host: &str) -> String {
    let inner = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(addr) = inner.parse::<std::net::Ipv6Addr>() {
        return addr.to_string();
    }
    let lower = inner.to_ascii_lowercase();
    lower.strip_suffix('.').map(str::to_owned).unwrap_or(lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, uri: &str) -> Request<()> {
        Request::builder().method(method).uri(uri).body(()).unwrap()
    }

    #[test]
    fn empty_route_matches_everything() {
        let route = Route {
            methods: vec![],
            host: None,
            path_prefix: None,
        };
        assert!(route.matches(&request("GET", "https://example.com/a")));
    }

    #[test]
    fn fields_are_anded_and_methods_are_case_insensitive() {
        let route = Route {
            methods: vec!["post".into()],
            host: Some("API.EXAMPLE.COM".into()),
            path_prefix: Some("/v1/code/".into()),
        };
        assert!(route.matches(&request(
            "POST",
            "https://api.example.com/v1/code/sessions?q=1"
        )));
        assert!(!route.matches(&request("GET", "https://api.example.com/v1/code/sessions")));
        assert!(!route.matches(&request("POST", "https://api.example.com/v1/messages")));
    }

    #[test]
    fn host_header_is_used_for_origin_form_requests() {
        let route = Route {
            methods: vec![],
            host: Some("example.com".into()),
            path_prefix: Some("/bridge".into()),
        };
        let request = Request::builder()
            .uri("/bridge?id=1")
            .header("host", "example.com:443")
            .body(())
            .unwrap();
        assert!(route.matches(&request));
    }

    #[test]
    fn ipv6_host_header_with_port_matches_bracket_free_route_host() {
        // A bracketed IPv6 literal with a port in the Host header must not be
        // mangled by naive `split(':')` port-stripping (which would yield "[").
        let route = Route {
            methods: vec![],
            host: Some("::1".into()),
            path_prefix: None,
        };
        let request = Request::builder()
            .uri("/x")
            .header("host", "[::1]:8080")
            .body(())
            .unwrap();
        assert!(route.matches(&request));
    }

    #[test]
    fn trailing_dot_fqdn_matches_bare_configured_host() {
        // "example.com." (absolute/FQDN form) must be treated as equivalent
        // to "example.com" when matching against a configured route host.
        let route = Route {
            methods: vec![],
            host: Some("example.com".into()),
            path_prefix: None,
        };
        let request = Request::builder()
            .uri("/x")
            .header("host", "example.com.")
            .body(())
            .unwrap();
        assert!(route.matches(&request));
    }

    #[test]
    fn ipv6_shorthand_host_matches_expanded_route_host() {
        // "[::1]" and "0:0:0:0:0:0:0:1" denote the same address and must
        // compare equal once normalized, regardless of which side is terse.
        let route = Route {
            methods: vec![],
            host: Some("0:0:0:0:0:0:0:1".into()),
            path_prefix: None,
        };
        let request = Request::builder()
            .uri("/x")
            .header("host", "[::1]:8080")
            .body(())
            .unwrap();
        assert!(route.matches(&request));
    }
}
