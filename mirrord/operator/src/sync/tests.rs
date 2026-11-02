use super::{
    AppReport, AppState, EntryCounts, EntryPlan, InstallReport, InstallState, ManifestKind,
    ManifestPath, PushMode, RejectReason, SidecarState, SupervisorReport, SyncFrame, UnpairReason,
    chunked,
    rules::{IgnoreSnapshot, IgnoreSource},
    wanted_path_frame_cost,
};

/// The CLI, the operator and the sidecar come from different releases, so each frame must keep
/// its bytes: a variant inserted before another one, or a field added to a variant, would make
/// older peers misread it. The bytes follow `bincode`'s standard configuration: the variant's
/// index first, then the fields in order, with integers as varints (one byte up to 250, `251` and
/// two little-endian bytes above) and vectors and strings prefixed with their length.
#[test]
fn frames_keep_their_encoding() {
    let plan = EntryPlan {
        index: 2,
        files: 3,
        directories: 4,
        bytes: 300,
        rules: IgnoreSnapshot {
            prefix: b"a".to_vec(),
            sources: vec![IgnoreSource {
                root: Vec::new(),
                lines: vec!["x".to_owned()],
            }],
        },
    };

    let frames: [(SyncFrame, &[u8]); 28] = [
        (
            SyncFrame::Begin {
                push_id: 7,
                mode: PushMode::Full,
                entries: vec![plan],
            },
            &[0, 7, 0, 1, 2, 3, 4, 251, 44, 1, 1, b'a', 1, 0, 1, 1, b'x'],
        ),
        (
            SyncFrame::Begin {
                push_id: 7,
                mode: PushMode::Incremental { base_generation: 5 },
                entries: Vec::new(),
            },
            &[0, 7, 1, 5, 0],
        ),
        (
            SyncFrame::Data {
                index: 1,
                chunk: vec![0xAB, 0xCD],
            },
            &[1, 1, 2, 0xAB, 0xCD],
        ),
        (
            SyncFrame::EntryDone {
                index: 1,
                deleted: vec![b"a/b".to_vec(), vec![0xFF]],
            },
            &[2, 1, 2, 3, b'a', b'/', b'b', 1, 0xFF],
        ),
        (SyncFrame::Commit { push_id: 7 }, &[3, 7]),
        (
            SyncFrame::Applied {
                push_id: 7,
                generation: 300,
                entries: vec![EntryCounts {
                    index: 0,
                    written: 1,
                    unchanged: 2,
                    deleted: 3,
                }],
            },
            &[4, 7, 251, 44, 1, 1, 0, 1, 2, 3],
        ),
        (
            SyncFrame::Rejected {
                push_id: 7,
                reason: RejectReason::StaleBase,
            },
            &[5, 7, 0],
        ),
        (
            SyncFrame::Rejected {
                push_id: 7,
                reason: RejectReason::Conflict("d".to_owned()),
            },
            &[5, 7, 3, 1, b'd'],
        ),
        (
            SyncFrame::Rejected {
                push_id: 7,
                reason: RejectReason::Io("e".to_owned()),
            },
            &[5, 7, 6, 1, b'e'],
        ),
        (SyncFrame::SidecarState(SidecarState::Disconnected), &[6, 0]),
        (
            SyncFrame::SidecarState(SidecarState::WaitingForVolume),
            &[6, 1],
        ),
        (SyncFrame::SidecarState(SidecarState::Seeding), &[6, 2]),
        (
            SyncFrame::SidecarState(SidecarState::AwaitingFirstPush),
            &[6, 3],
        ),
        (
            SyncFrame::SidecarState(SidecarState::Synced { generation: 3 }),
            &[6, 4, 3],
        ),
        (
            SyncFrame::SidecarState(SidecarState::Failed {
                message: "no".to_owned(),
            }),
            &[6, 5, 2, b'n', b'o'],
        ),
        (
            SyncFrame::Status(SidecarState::Synced { generation: 3 }),
            &[7, 4, 3],
        ),
        (
            SyncFrame::Paired {
                generation: None,
                protocol_version: 1,
            },
            &[8, 0, 1],
        ),
        (
            SyncFrame::Paired {
                generation: Some(3),
                protocol_version: 1,
            },
            &[8, 1, 3, 1],
        ),
        (
            SyncFrame::Error {
                message: "gone".to_owned(),
            },
            &[9, 4, b'g', b'o', b'n', b'e'],
        ),
        (
            // Signed integers are zigzag varints, so the exit code 1 is `2`.
            SyncFrame::Supervisor(SupervisorReport {
                install: InstallReport {
                    state: InstallState::Failed,
                    error: Some("x".to_owned()),
                },
                app: AppReport {
                    state: AppState::Restarting,
                    restarts: 2,
                    last_exit_code: Some(1),
                },
                handled: Some(4),
            }),
            &[10, 3, 1, 1, b'x', 2, 2, 1, 2, 1, 4],
        ),
        (
            SyncFrame::Unpaired {
                reason: UnpairReason::Replaced,
            },
            &[11, 3],
        ),
        (SyncFrame::WakeRequest, &[12]),
        (
            SyncFrame::Manifest {
                push_id: 7,
                index: 1,
                paths: vec![ManifestPath {
                    path: b"d".to_vec(),
                    kind: ManifestKind::Dir,
                }],
            },
            &[13, 7, 1, 1, 1, b'd', 0],
        ),
        (
            SyncFrame::Manifest {
                push_id: 7,
                index: 1,
                paths: vec![ManifestPath {
                    path: b"f".to_vec(),
                    kind: ManifestKind::File {
                        mode: 0o644,
                        size: 2,
                        hash: [9; 32],
                    },
                }],
            },
            &[
                13, 7, 1, 1, 1, b'f', 1, 251, 0xA4, 0x01, 2, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,
                9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9, 9,
            ],
        ),
        (
            SyncFrame::Manifest {
                push_id: 7,
                index: 1,
                paths: vec![ManifestPath {
                    path: b"l".to_vec(),
                    kind: ManifestKind::Symlink {
                        target: b"t".to_vec(),
                    },
                }],
            },
            &[13, 7, 1, 1, 1, b'l', 2, 1, b't'],
        ),
        (SyncFrame::ManifestDone { push_id: 7 }, &[14, 7]),
        (
            SyncFrame::Want {
                push_id: 7,
                index: 1,
                paths: vec![b"f".to_vec()],
            },
            &[15, 7, 1, 1, 1, b'f'],
        ),
        (SyncFrame::WantDone { push_id: 7 }, &[16, 7]),
    ];

    for (frame, bytes) in frames {
        assert_eq!(frame.encode(), bytes, "{frame:?}");
        assert_eq!(SyncFrame::decode(bytes).unwrap(), frame);
    }
}

/// Every peer decodes frames that the other side sends, so a frame that claims a huge chunk must
/// fail to decode rather than make the decoder allocate the claimed size and abort the process.
#[test]
fn frames_that_claim_huge_lengths_fail_to_decode() {
    // A `Data` frame for entry 0 whose chunk claims 4 TiB: a varint `253` and eight
    // little-endian bytes of length.
    let mut frame = vec![1, 0, 253];
    frame.extend_from_slice(&(1u64 << 42).to_le_bytes());

    assert!(SyncFrame::decode(&frame).is_err());
}

/// A tree of many short paths fits in fewer bytes than [`MAX_FRAME_BYTES`] per frame, but the
/// decoder also claims the size of every path in memory against that limit, so a frame of such a
/// manifest, or of the sidecar's answer, must still decode.
#[test]
fn chunked_manifests_and_wants_decode() {
    let manifest = (0..20_000)
        .map(|index| ManifestPath {
            path: format!("d{index:05}").into_bytes(),
            kind: ManifestKind::Dir,
        })
        .collect::<Vec<_>>();
    let wanted = (0..50_000)
        .map(|index| format!("{index:05}").into_bytes())
        .collect::<Vec<_>>();

    let manifest_frames = chunked(manifest, ManifestPath::frame_cost)
        .into_iter()
        .map(|paths| SyncFrame::Manifest {
            push_id: 1,
            index: 0,
            paths,
        });
    let want_frames = chunked(wanted, |path| wanted_path_frame_cost(path))
        .into_iter()
        .map(|paths| SyncFrame::Want {
            push_id: 1,
            index: 0,
            paths,
        });

    for frame in manifest_frames.chain(want_frames) {
        assert_eq!(SyncFrame::decode(&frame.encode()).unwrap(), frame);
    }
}
