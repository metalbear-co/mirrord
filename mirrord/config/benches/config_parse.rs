use criterion::{Criterion, black_box, criterion_group, criterion_main};
use mirrord_config::{
    LayerFileConfig,
    config::{ConfigContext, MirrordConfig},
};

const BASIC_JSON_CONFIG: &str = r#"
{
    "target": "pod/test-service-abcdefg-abcd",
    "feature": {
        "env": true,
        "fs": "read",
        "network": {
            "dns": false,
            "incoming": "mirror",
            "outgoing": {
                "tcp": true,
                "udp": false
            }
        }
    }
}
"#;

const ADVANCED_JSON_CONFIG: &str = r#"
{
    "accept_invalid_certificates": false,
    "target": {
        "path": "pod/test-service-abcdefg-abcd",
        "namespace": "default"
    },
    "feature": {
        "env": true,
        "fs": "write",
        "network": {
            "dns": false,
            "incoming": {
                "mode": "steal",
                "http_filter": {
                    "header_filter": "x-intercept: test-value"
                }
            },
            "outgoing": {
                "tcp": true,
                "udp": false
            }
        }
    }
}
"#;

fn bench_parse_basic_config(c: &mut Criterion) {
    c.bench_function("parse_basic_json_config", |b| {
        b.iter(|| {
            let config: LayerFileConfig =
                serde_json::from_str(black_box(BASIC_JSON_CONFIG)).unwrap();
            black_box(config);
        });
    });
}

fn bench_parse_advanced_config(c: &mut Criterion) {
    c.bench_function("parse_advanced_json_config", |b| {
        b.iter(|| {
            let config: LayerFileConfig =
                serde_json::from_str(black_box(ADVANCED_JSON_CONFIG)).unwrap();
            black_box(config);
        });
    });
}

fn bench_generate_config(c: &mut Criterion) {
    c.bench_function("generate_layer_config", |b| {
        b.iter(|| {
            let file_config: LayerFileConfig =
                serde_json::from_str(black_box(BASIC_JSON_CONFIG)).unwrap();
            let mut ctx = ConfigContext::default();
            let config = file_config.generate_config(&mut ctx).unwrap();
            black_box(config);
        });
    });
}

fn bench_config_encode_decode(c: &mut Criterion) {
    let file_config: LayerFileConfig = serde_json::from_str(BASIC_JSON_CONFIG).unwrap();
    let mut ctx = ConfigContext::default();
    let config = file_config.generate_config(&mut ctx).unwrap();

    c.bench_function("config_encode_decode_roundtrip", |b| {
        b.iter(|| {
            let encoded = config.encode().unwrap();
            let decoded = mirrord_config::LayerConfig::decode(black_box(&encoded)).unwrap();
            black_box(decoded);
        });
    });
}

criterion_group!(
    benches,
    bench_parse_basic_config,
    bench_parse_advanced_config,
    bench_generate_config,
    bench_config_encode_decode,
);
criterion_main!(benches);
