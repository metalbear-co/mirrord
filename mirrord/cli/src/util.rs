/// Removes `HTTP_PROXY` and `https_proxy` from the environment
pub(crate) fn remove_proxy_env() {
    for (key, _val) in std::env::vars() {
        let lower_key = key.to_lowercase();
        if lower_key == "http_proxy" || lower_key == "https_proxy" {
            // we set instead of unset since this way extension
            // will be able to propogate it as well.
            std::env::set_var(key, "")
        }
    }
}
