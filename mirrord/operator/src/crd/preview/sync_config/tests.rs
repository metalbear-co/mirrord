use super::{
    PreviewSyncConfig, PreviewSyncConfigError, PreviewSyncEntry, PreviewSyncPrepare,
    PreviewSyncRestart, PreviewSyncRun,
};

/// Settings with `remotes` and `persist`, and with every other setting valid.
fn config(remotes: &[&str], persist: &[&str]) -> PreviewSyncConfig {
    PreviewSyncConfig {
        entries: remotes
            .iter()
            .map(|remote| PreviewSyncEntry {
                remote: (*remote).to_owned(),
            })
            .collect(),
        run: Some(PreviewSyncRun {
            command: vec!["npm".to_owned(), "run".to_owned(), "dev".to_owned()],
            working_dir: Some("/app".to_owned()),
            restart: PreviewSyncRestart::OnSync,
        }),
        prepare: Some(PreviewSyncPrepare {
            command: vec!["npm".to_owned(), "ci".to_owned()],
            triggered_by: vec![
                "/app/package-lock.json".to_owned(),
                "/app/**/requirements*.txt".to_owned(),
            ],
            persist: persist.iter().map(|path| (*path).to_owned()).collect(),
            working_dir: Some("/app".to_owned()),
        }),
    }
}

/// The pod mounts each `remote` and `prepare.persist` path from the code volume, so a path that
/// is not absolute, that another mount or a path of the operator's would hide, or that would hide
/// the whole image, must be refused before the operator builds the pod.
#[test]
fn check_refuses_paths_that_the_pod_cannot_mount() {
    let accepted = [
        config(&["/app"], &[]),
        config(
            &["/app", "/application", "/srv/lib"],
            &["/usr/local/lib/python3.12/site-packages"],
        ),
        // Only the operator's own paths are reserved, not every path that starts like one.
        config(&["/mirrordx", "/mirrord-other"], &[]),
    ];

    for config in accepted {
        assert_eq!(config.check(), Ok(()), "{config:?}");
    }

    let not_normalized = |option: &'static str, path: &str| PreviewSyncConfigError::NotNormalized {
        option,
        path: path.to_owned(),
    };
    let overlap =
        |first_option, first: &str, second_option, second: &str| PreviewSyncConfigError::Overlap {
            first_option,
            first: first.to_owned(),
            second_option,
            second: second.to_owned(),
        };

    let refused = [
        (
            config(&["app"], &[]),
            not_normalized("sync.files[].remote", "app"),
        ),
        (
            config(&["/app/"], &[]),
            not_normalized("sync.files[].remote", "/app/"),
        ),
        (
            config(&["//app"], &[]),
            not_normalized("sync.files[].remote", "//app"),
        ),
        (
            config(&["/app/../etc"], &[]),
            not_normalized("sync.files[].remote", "/app/../etc"),
        ),
        (
            config(&["/app", "/srv/./lib"], &[]),
            not_normalized("sync.files[].remote", "/srv/./lib"),
        ),
        (
            config(&["/app"], &["lib"]),
            not_normalized("sync.prepare.persist", "lib"),
        ),
        (
            config(&["/"], &[]),
            PreviewSyncConfigError::Root {
                option: "sync.files[].remote",
            },
        ),
        (
            config(&["/mirrord/bin"], &[]),
            PreviewSyncConfigError::Reserved {
                option: "sync.files[].remote",
                path: "/mirrord/bin".to_owned(),
                reserved: "/mirrord",
            },
        ),
        (
            config(&["/app"], &["/mirrord-code"]),
            PreviewSyncConfigError::Reserved {
                option: "sync.prepare.persist",
                path: "/mirrord-code".to_owned(),
                reserved: "/mirrord-code",
            },
        ),
        (
            config(&["/app", "/app"], &[]),
            overlap("sync.files[].remote", "/app", "sync.files[].remote", "/app"),
        ),
        (
            config(&["/app/src", "/app"], &[]),
            overlap(
                "sync.files[].remote",
                "/app/src",
                "sync.files[].remote",
                "/app",
            ),
        ),
        (
            config(&["/app"], &["/app/node_modules"]),
            overlap(
                "sync.files[].remote",
                "/app",
                "sync.prepare.persist",
                "/app/node_modules",
            ),
        ),
    ];

    for (config, error) in refused {
        assert_eq!(config.check(), Err(error), "{config:?}");
    }
}

/// The pod can't start the app or run `prepare` without a command, runs `prepare` only under the
/// supervisor that `run` starts, and resolves the working directories and the `triggered_by` globs
/// in the container, so they must be absolute.
#[test]
fn check_refuses_commands_and_container_paths_the_pod_cannot_use() {
    let no_entries = config(&[], &[]);
    assert_eq!(no_entries.check(), Err(PreviewSyncConfigError::NoEntries));

    let mut no_run = config(&["/app"], &[]);
    no_run.prepare = None;
    no_run.run = None;
    assert_eq!(no_run.check(), Ok(()));

    let mut prepare_without_run = config(&["/app"], &[]);
    prepare_without_run.run = None;
    assert_eq!(
        prepare_without_run.check(),
        Err(PreviewSyncConfigError::PrepareWithoutRun)
    );

    let mut no_run_command = config(&["/app"], &[]);
    no_run_command.run.as_mut().unwrap().command.clear();
    assert_eq!(
        no_run_command.check(),
        Err(PreviewSyncConfigError::EmptyRunCommand)
    );

    let mut no_prepare_command = config(&["/app"], &[]);
    no_prepare_command.prepare.as_mut().unwrap().command.clear();
    assert_eq!(
        no_prepare_command.check(),
        Err(PreviewSyncConfigError::EmptyPrepareCommand)
    );

    let mut relative_working_dir = config(&["/app"], &[]);
    relative_working_dir.run.as_mut().unwrap().working_dir = Some("app".to_owned());
    assert_eq!(
        relative_working_dir.check(),
        Err(PreviewSyncConfigError::NotNormalized {
            option: "sync.run.directory",
            path: "app".to_owned(),
        })
    );

    let mut relative_prepare_dir = config(&["/app"], &[]);
    relative_prepare_dir.prepare.as_mut().unwrap().working_dir = Some("./app".to_owned());
    assert_eq!(
        relative_prepare_dir.check(),
        Err(PreviewSyncConfigError::NotNormalized {
            option: "sync.prepare.directory",
            path: "./app".to_owned(),
        })
    );

    let mut relative_glob = config(&["/app"], &[]);
    relative_glob.prepare.as_mut().unwrap().triggered_by = vec!["package-lock.json".to_owned()];
    assert_eq!(
        relative_glob.check(),
        Err(PreviewSyncConfigError::NotNormalized {
            option: "sync.prepare.triggered_by",
            path: "package-lock.json".to_owned(),
        })
    );

    let mut root_working_dir = config(&["/app"], &[]);
    // Unlike a mount, the working directory can be `/`, because it hides nothing.
    root_working_dir.run.as_mut().unwrap().working_dir = Some("/".to_owned());
    assert_eq!(root_working_dir.check(), Ok(()));
}
