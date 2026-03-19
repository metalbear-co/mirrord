use kube::{CustomResource, Resource, api::ObjectMeta};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Generic configuration properties related to mirrord.
///
/// Modeled after environment variables in container specs, properties can be declared inline or
/// loaded from ConfigMaps/Secrets.
#[derive(
    CustomResource, Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash, Default,
)]
#[kube(
    group = "mirrord.metalbear.co",
    version = "v1",
    kind = "MirrordPropertyList",
    shortname = "mpl",
    namespaced
)]
#[serde(rename_all = "camelCase")]
pub struct MirrordPropertyListSpec {
    /// Properties declared inline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<Property>,
    /// Properties loaded from other resources.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties_from: Vec<PropertiesFrom>,
}

/// Property declared inline.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct Property {
    /// Name of the property.
    pub name: String,
    /// Value of the property.
    #[serde(flatten)]
    pub value: PropertyValue,
}

/// Specifies how to load a property value from some resource.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum PropertyValue {
    /// Value declared inline.
    Value(String),
    /// Value loaded from some other resource.
    ValueFrom(PropertyValueFrom),
}

/// Specifies how to load a property value from some resource.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum PropertyValueFrom {
    /// Value loaded from a ConfigMap key.
    ConfigMapKeyRef(ResourceKeyRef),
    /// Value loaded from a Secret key.
    SecretKeyRef(ResourceKeyRef),
}

/// Specifies how to pull property value from some resource of a known kind.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ResourceKeyRef {
    /// Name of the resource.
    pub name: String,
    /// Key within the resource.
    pub key: String,
    /// Determines how we handle the case when the key or the resource is not found.
    #[serde(default)]
    pub optional: bool,
}

/// Properties loaded from some resource.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct PropertiesFrom {
    /// Optional prefix to be added to the names of all loaded properties.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// Load properties from some other resource.
    #[serde(flatten)]
    pub from: PropertiesFromRef,
}

/// Specifies hwo to load properties from some resource.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum PropertiesFromRef {
    /// Load properties from a ConfigMap.
    ConfigMapRef(ResourceRef),
    /// Load properties from a Secret.
    SecretRef(ResourceRef),
}

/// Specifies how to pull properties from some resource of a known kind.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRef {
    /// Name of the resource.
    pub name: String,
    /// Determines how we handle the case when the resource is not found.
    #[serde(default)]
    pub optional: bool,
}

/// Forward compatibility wrapper for [`MirrordPropertyList`], inherits its [`Resource`]
/// implementation.
///
/// Should be used for all list/watch requests.
#[derive(Resource, Deserialize, Clone, Debug)]
#[resource(inherit = "MirrordPropertyList")]
pub struct MirrordPropertyListCompat {
    pub metadata: ObjectMeta,
    pub spec: MirrordPropertyListSpecCompat,
}

#[derive(Deserialize, Clone, Debug)]
pub enum MirrordPropertyListSpecCompat {
    Known(MirrordPropertyListSpec),
    Unknown(serde_json::Value),
}
