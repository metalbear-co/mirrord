use std::env;

fn main() {
    let test_env_key = "TEST";
    let expected_val = "legit";

    set_env_var(test_env_key, expected_val);
    assert_eq!(get_env_var(test_env_key), expected_val);
}

fn set_env_var(key: &str, val: &str) {
    env::set_var(key, val);
    println!("setting env:{key}={val:#?}");
}

fn get_env_var(key: &str) -> String {
    let val = env::var(key).expect("expecting env var to exists");
    println!("got env:{key}={val}");
    val
}
