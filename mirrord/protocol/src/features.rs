use std::{cell::RefCell, fmt, str::FromStr};

use bitflags::bitflags;
use bitflags_serde_shim::impl_serde_for_bitflags;
use serde::{Deserialize, Serialize};

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Features: u32 { }
}

impl_serde_for_bitflags!(Features);

impl fmt::Display for Features {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl FromStr for Features {
    type Err = bitflags::parser::ParseError;

    fn from_str(flags: &str) -> Result<Self, Self::Err> {
        Ok(Self(flags.parse()?))
    }
}

thread_local!(
    static PROTOCOL_FEATURES: RefCell<Features> = RefCell::new(Features::empty())
);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequireFeature<const F: u32, T>(Option<T>);

impl<const F: u32, T> Serialize for RequireFeature<F, T>
where
    Option<T>: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        let allowed = PROTOCOL_FEATURES.with(|cell| {
            Features::from_bits(F)
                .map(|flags| *cell.borrow() & flags == flags)
                .unwrap_or(false)
        });

        if allowed {
            self.0.serialize(serializer)
        } else {
            ().serialize(serializer)
        }
    }
}

impl<'de, const F: u32, T> Deserialize<'de> for RequireFeature<F, T>
where
    Option<T>: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let allowed = PROTOCOL_FEATURES.with(|cell| {
            Features::from_bits(F)
                .map(|flags| *cell.borrow() & flags == flags)
                .unwrap_or(false)
        });

        if allowed {
            Option::<T>::deserialize(deserializer).map(RequireFeature)
        } else {
            Ok(RequireFeature(None))
        }
    }
}

pub mod with_bincode {
    use bincode::{
        config::Config,
        error::{DecodeError, EncodeError},
        Decode, Encode,
    };

    use super::{Features, PROTOCOL_FEATURES};

    pub fn encode_with_features<S, C>(
        features: Features,
        value: S,
        config: C,
    ) -> Result<Vec<u8>, EncodeError>
    where
        S: Encode,
        C: Config,
    {
        PROTOCOL_FEATURES.with(|cell| {
            if let Ok(mut feat) = cell.try_borrow_mut() {
                *feat = features
            }
        });

        let result = bincode::encode_to_vec(value, config);

        PROTOCOL_FEATURES.with(|cell| {
            if let Ok(mut feat) = cell.try_borrow_mut() {
                *feat = Features::empty()
            }
        });

        result
    }

    pub fn decode_with_features<D, C>(
        features: Features,
        buffer: &[u8],
        config: C,
    ) -> Result<(D, usize), DecodeError>
    where
        C: Config,
        D: Decode,
    {
        PROTOCOL_FEATURES.with(|cell| {
            if let Ok(mut feat) = cell.try_borrow_mut() {
                *feat = features
            }
        });

        let result = bincode::decode_from_slice(buffer, config);

        PROTOCOL_FEATURES.with(|cell| {
            if let Ok(mut feat) = cell.try_borrow_mut() {
                *feat = Features::empty()
            }
        });

        result
    }
}
