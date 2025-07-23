use mirrord_win::{read_config, run_targetless};

fn main() {
    let cfg = read_config();

    let output =
        run_targetless(cfg.target_commandline, cfg.layer_dll_path).expect("Failed run_targetless");
    println!("target stdout: {output}");
}
