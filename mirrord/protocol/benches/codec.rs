use actix_codec::{Decoder, Encoder};
use bytes::BytesMut;
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use mirrord_protocol::{
    ClientMessage, DaemonMessage, Payload,
    codec::{ClientCodec, DaemonCodec},
    tcp::{DaemonTcp, LayerTcp, TcpData},
};

fn make_client_message() -> ClientMessage {
    ClientMessage::Tcp(LayerTcp::PortSubscribe(8080))
}

fn make_daemon_message_small() -> DaemonMessage {
    DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
        connection_id: 1,
        bytes: Payload::from(vec![1u8; 64]),
    }))
}

fn make_daemon_message_large() -> DaemonMessage {
    DaemonMessage::Tcp(DaemonTcp::Data(TcpData {
        connection_id: 1,
        bytes: Payload::from(vec![0xABu8; 8192]),
    }))
}

fn bench_client_encode(c: &mut Criterion) {
    let msg = make_client_message();
    let mut codec = ClientCodec::default();

    c.bench_function("client_codec_encode", |b| {
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(256);
            codec.encode(black_box(msg.clone()), &mut buf).unwrap();
            black_box(buf);
        });
    });
}

fn bench_client_roundtrip(c: &mut Criterion) {
    let msg = make_client_message();
    let mut client_codec = ClientCodec::default();
    let mut daemon_codec = DaemonCodec::default();

    c.bench_function("client_codec_roundtrip", |b| {
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(256);
            client_codec
                .encode(black_box(msg.clone()), &mut buf)
                .unwrap();
            let decoded = daemon_codec.decode(&mut buf).unwrap().unwrap();
            black_box(decoded);
        });
    });
}

fn bench_daemon_encode_small(c: &mut Criterion) {
    let msg = make_daemon_message_small();
    let mut codec = DaemonCodec::default();

    c.bench_function("daemon_codec_encode_small_payload", |b| {
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(256);
            codec.encode(black_box(msg.clone()), &mut buf).unwrap();
            black_box(buf);
        });
    });
}

fn bench_daemon_roundtrip_small(c: &mut Criterion) {
    let msg = make_daemon_message_small();
    let mut daemon_codec = DaemonCodec::default();
    let mut client_codec = ClientCodec::default();

    c.bench_function("daemon_codec_roundtrip_small_payload", |b| {
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(256);
            daemon_codec
                .encode(black_box(msg.clone()), &mut buf)
                .unwrap();
            let decoded = client_codec.decode(&mut buf).unwrap().unwrap();
            black_box(decoded);
        });
    });
}

fn bench_daemon_encode_large(c: &mut Criterion) {
    let msg = make_daemon_message_large();
    let mut codec = DaemonCodec::default();

    c.bench_function("daemon_codec_encode_large_payload", |b| {
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(16384);
            codec.encode(black_box(msg.clone()), &mut buf).unwrap();
            black_box(buf);
        });
    });
}

fn bench_daemon_roundtrip_large(c: &mut Criterion) {
    let msg = make_daemon_message_large();
    let mut daemon_codec = DaemonCodec::default();
    let mut client_codec = ClientCodec::default();

    c.bench_function("daemon_codec_roundtrip_large_payload", |b| {
        b.iter(|| {
            let mut buf = BytesMut::with_capacity(16384);
            daemon_codec
                .encode(black_box(msg.clone()), &mut buf)
                .unwrap();
            let decoded = client_codec.decode(&mut buf).unwrap().unwrap();
            black_box(decoded);
        });
    });
}

criterion_group!(
    benches,
    bench_client_encode,
    bench_client_roundtrip,
    bench_daemon_encode_small,
    bench_daemon_roundtrip_small,
    bench_daemon_encode_large,
    bench_daemon_roundtrip_large,
);
criterion_main!(benches);
