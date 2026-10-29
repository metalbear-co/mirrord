use super::{SidecarState, SyncFrame};

/// The CLI, the operator and the sidecar come from different releases, so each frame must keep
/// its bytes: a variant inserted before another one, or a field added to a variant, would make
/// older peers misread it. The bytes follow `bincode`'s standard configuration: the variant's
/// index first, then the fields in order, with integers as varints (one byte up to 250, `251` and
/// two little-endian bytes above) and vectors and strings prefixed with their length.
#[test]
fn frames_keep_their_encoding() {
    let frames: [(SyncFrame, &[u8]); 12] = [
        (
            SyncFrame::Begin {
                push_id: 7,
                entries: vec![0, 2],
            },
            &[0, 7, 2, 0, 2],
        ),
        (
            SyncFrame::Data {
                index: 1,
                chunk: vec![0xAB, 0xCD],
            },
            &[1, 1, 2, 0xAB, 0xCD],
        ),
        (SyncFrame::Commit { push_id: 7 }, &[2, 7]),
        (
            SyncFrame::Applied {
                push_id: 7,
                generation: 300,
            },
            &[3, 7, 251, 44, 1],
        ),
        (
            SyncFrame::Rejected {
                push_id: 7,
                reason: "bad".to_owned(),
            },
            &[4, 7, 3, b'b', b'a', b'd'],
        ),
        (SyncFrame::SidecarState(SidecarState::Disconnected), &[5, 0]),
        (
            SyncFrame::SidecarState(SidecarState::AwaitingFirstPush),
            &[5, 1],
        ),
        (
            SyncFrame::SidecarState(SidecarState::Synced { generation: 3 }),
            &[5, 2, 3],
        ),
        (
            SyncFrame::Status(SidecarState::Synced { generation: 3 }),
            &[6, 2, 3],
        ),
        (SyncFrame::Paired { generation: None }, &[7, 0]),
        (
            SyncFrame::Paired {
                generation: Some(3),
            },
            &[7, 1, 3],
        ),
        (
            SyncFrame::Error {
                message: "gone".to_owned(),
            },
            &[8, 4, b'g', b'o', b'n', b'e'],
        ),
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
