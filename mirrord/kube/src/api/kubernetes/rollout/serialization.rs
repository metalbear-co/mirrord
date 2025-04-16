//! Manual implementation of serialization and deserialization of the [`Rollout`].
//!
//! Some fields of the [`Rollout`] are actually associated fields of [`Resource`], so they're simply
//! not serialized if we just use `#[derive(Serialize, Deserialize)]`.
//!
//! The implementation here is largely _inspired_ by how `Deployment` is (de)serialized in
//! [`k8s_openapi`].
use std::fmt;

use k8s_openapi::Resource;
use serde::{
    de::{self, MapAccess, Visitor},
    ser::SerializeStruct,
    Deserialize, Serialize,
};

use crate::api::kubernetes::rollout::Rollout;

impl Serialize for Rollout {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct(
            "Rollout",
            3 + self.spec.as_ref().map_or(0, |_| 1) + self.status.as_ref().map_or(0, |_| 1),
        )?;
        state.serialize_field("apiVersion", Rollout::API_VERSION)?;
        state.serialize_field("kind", Rollout::KIND)?;
        state.serialize_field("metadata", &self.metadata)?;
        if let Some(spec) = &self.spec {
            state.serialize_field("spec", spec)?;
        }
        if let Some(status) = &self.status {
            state.serialize_field("status", status)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for Rollout {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        enum Field {
            ApiVersion,
            Kind,
            Metadata,
            Spec,
            Status,
            Other,
        }

        impl<'de> Deserialize<'de> for Field {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct FieldVisitor;

                impl Visitor<'_> for FieldVisitor {
                    type Value = Field;

                    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                        formatter.write_str("`apiVersion`, `kind`, `metadata`, `spec`, or `status`")
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: de::Error,
                    {
                        Ok(match value {
                            "apiVersion" => Field::ApiVersion,
                            "kind" => Field::Kind,
                            "metadata" => Field::Metadata,
                            "spec" => Field::Spec,
                            "status" => Field::Status,
                            _ => Field::Other,
                        })
                    }
                }

                deserializer.deserialize_identifier(FieldVisitor)
            }
        }

        struct RolloutVisitor;

        impl<'de> Visitor<'de> for RolloutVisitor {
            type Value = Rollout;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("struct Rollout")
            }

            fn visit_map<V>(self, mut map: V) -> Result<Self::Value, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut metadata = None;
                let mut spec = None;
                let mut status = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::ApiVersion => {
                            let api_version: String = map.next_value()?;
                            if api_version != Rollout::API_VERSION {
                                return Err(de::Error::invalid_value(
                                    de::Unexpected::Str(&api_version),
                                    &Rollout::API_VERSION,
                                ));
                            }
                        }
                        Field::Kind => {
                            let kind: String = map.next_value()?;
                            if kind != Rollout::KIND {
                                return Err(de::Error::invalid_value(
                                    de::Unexpected::Str(&kind),
                                    &Rollout::KIND,
                                ));
                            }
                        }
                        Field::Metadata => {
                            metadata = Some(map.next_value()?);
                        }
                        Field::Spec => {
                            spec = Some(map.next_value()?);
                        }
                        Field::Status => {
                            status = Some(map.next_value()?);
                        }
                        Field::Other => {
                            let _: de::IgnoredAny = map.next_value()?;
                        }
                    }
                }

                Ok(Rollout {
                    metadata: metadata.ok_or_else(|| de::Error::missing_field("metadata"))?,
                    spec,
                    status,
                })
            }
        }

        const FIELDS: &[&str] = &["apiVersion", "kind", "metadata", "spec", "status"];
        deserializer.deserialize_struct("Rollout", FIELDS, RolloutVisitor)
    }
}
