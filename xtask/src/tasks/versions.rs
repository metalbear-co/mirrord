pub const UV_VERSION: &str = "0.11.3";
pub const CARGO_ZIGBUILD_VERSION: &str = "0.22.1";

pub fn cargo_zigbuild_hash() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => {
            Some("d24545d1595738c7be21be37b61122814216893b3df79db41476477f2141f438")
        }
        ("linux", "aarch64") => {
            Some("22fb45a099398e4f79bd8fbe1a3a23d51ee2b870c1d9c6b04b120e1db421f91b")
        }
        ("macos", "x86_64") | ("macos", "aarch64") => {
            Some("9cf70fe0e9edad06aef81c8fd044223d29c8ceab0a5624bb605a784e77578d6a")
        }
        ("windows", "x86_64") => {
            Some("0549be58f9d0aac3383b3afb26170b69ebf50374bcb0ab53778375c91d38ba9c")
        }
        ("windows", "aarch64") => {
            Some("0577795e982b894ac0b8727127c6533e1d3f172a68bec228e42078c87786032f")
        }
        _ => None,
    }
}

pub fn uv_hash() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => {
            Some("0fde893b5ab9f6997fe357138e794bac09d144328052519fbbe2e6f72145e457")
        }
        ("linux", "aarch64") => {
            Some("45006bcd9e8718248a23ab81448a5beb46a72a9dd508e3212d6f3b8c63aeb88a")
        }
        ("macos", "x86_64") => {
            Some("d2b3b0fa1693880ca354755c216ae1c65dd938a4f1a24374d0c3f4b9538e0ee6")
        }
        ("macos", "aarch64") => {
            Some("71f5d0b9e73daa5d8a7e2db3fa2e22a4537d24bb4fe78130db797280280d4edc")
        }
        ("windows", "x86_64") => {
            Some("68fda574f2e5e7536a2b747dcea88329a71aad7222317e8f4717d0af8f99fbd4")
        }
        ("windows", "aarch64") => {
            Some("92ffc4d521ab2c4738ef05d8ef26f2750e26d31f3ad5611cdfefc52445be9ace")
        }
        _ => None,
    }
}
