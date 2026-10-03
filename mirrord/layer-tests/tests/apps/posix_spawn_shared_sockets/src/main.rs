use std::process::Command;

const SHARED_SOCKETS_ENV_VAR: &str = "MIRRORD_SHARED_SOCKETS";

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        assert!(std::env::var_os(SHARED_SOCKETS_ENV_VAR).is_some());
        return;
    }

    let executable = std::env::current_exe().unwrap();
    // `Command` without a pre-exec hook uses glibc's `posix_spawn`, which runs `execve` from a
    // `vfork` child on this thread. The second child only gets the variable if the first `execve`
    // left no replacement state behind on this thread.
    for _ in 0..2 {
        assert!(
            Command::new(&executable)
                .arg("child")
                .status()
                .unwrap()
                .success()
        );
    }
}
