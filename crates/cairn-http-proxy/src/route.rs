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
                .is_some_and(|actual| actual.eq_ignore_ascii_case(expected.as_str()))
        });
        let path_matches = self
            .path_prefix
            .as_ref()
            .is_none_or(|prefix| request.uri().path().starts_with(prefix));
        method_matches && host_matches && path_matches
    }
}

fn request_host<B>(request: &Request<B>) -> Option<&str> {
    request.uri().host().or_else(|| {
        request
            .headers()
            .get(http::header::HOST)
            .and_then(|value| value.to_str().ok())
            .and_then(|authority| authority.split(':').next())
    })
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
}
