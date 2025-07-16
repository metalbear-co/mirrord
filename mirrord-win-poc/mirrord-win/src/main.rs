use mirrord_win::{read_config, run_targetless};

fn main() {
    let cfg = read_config();

    run_targetless(cfg.target_commandline, cfg.layer_dll_path);
}
