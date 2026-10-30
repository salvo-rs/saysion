#[cfg(test)]
mod tests {
    use mongodb::{
        Client,
        options::{ClientOptions, ServerAddress, Socks5Proxy},
    };
    use saysion::MongoDbStore;

    #[test]
    fn downstream_proxy_credentials_never_appear_in_store_debug()
    -> Result<(), Box<dyn std::error::Error>> {
        // Only enter a current-thread runtime so Client can queue background
        // tasks. Never drive it: no monitoring task or network I/O is polled,
        // and no MongoDB server or proxy is required by this regression test.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let _entered = runtime.enter();
        let username = "fictional-proxy-debug-user";
        let password = "fictional-proxy-debug-password";
        let mut options = ClientOptions::default();
        options.hosts = vec![ServerAddress::parse("127.0.0.1:9")?];
        options.socks5_proxy = Some(
            Socks5Proxy::builder()
                .host("127.0.0.1")
                .port(Some(9_u16))
                .authentication((username.to_string(), password.to_string()))
                .build(),
        );
        let store = MongoDbStore::new(Client::with_options(options)?, "fixture_db")
            .with_collection("fixture_sessions");
        for output in [format!("{store:?}"), format!("{store:#?}")] {
            assert!(output.contains("MongoDbStore"));
            assert!(output.contains("fixture_db"));
            assert!(output.contains("fixture_sessions"));
            assert!(!output.contains(username), "proxy username leaked");
            assert!(!output.contains(password), "proxy password leaked");
        }
        Ok(())
    }
}
