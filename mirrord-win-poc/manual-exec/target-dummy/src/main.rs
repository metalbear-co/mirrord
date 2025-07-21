//! target-dummy for loading poc tests
//! commandline: `target-dummy.exe KeyName KeyValue`
//!
//! sets Env Variable KeyName to KeyValue
//! prints: Result of Env. Variable of setting (should be equal to KeyValue, or is it? :smug-face:)
use std::env;

fn main() {
    let mut args = env::args().peekable();
    // print!("{:?}", &args);

    // consume executable if exists
    let _ = args.next_if(|arg| arg.ends_with("exe"));
    let env_key = args.next().expect("missing Env. Key");
    let val = args.next().expect("missing Value to set");

    set_env_var(&env_key, &val);
    println!("{:}", get_env_var(&env_key));
}

fn set_env_var(key: &str, val: &str) {
    env::set_var(key, val);
    // println!("setting env:{key}={val:#?}");
}

fn get_env_var(key: &str) -> String {
    let val = env::var(key).expect("expecting env var to exists");
    // println!("got env:{key}={val}");
    val
}
