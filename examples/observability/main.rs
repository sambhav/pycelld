//! Development-only example: allow requests to the local demonstration backend.
fn main() -> anyhow::Result<()> {
    pycelld::run(pycelld::Monty::new().with_fetch_middleware(|_, request| {
        if request.url.host_str() == Some("127.0.0.1") {
            pycelld::FetchDecision::Forward(request)
        } else {
            pycelld::FetchDecision::Deny("example permits only 127.0.0.1".into())
        }
    }))
}
