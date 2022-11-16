# Mirrord Config Derive

This crate implements the derive macro `MirrordConfig`, which introduces the `config` attribute:

- On a struct:
    - `map_to = &str` set the name of the new generated struct (defaults to `format!("File{}", <struct name>)`).
    - `derive = &str` extend `derive` statement with extra values.
    - `generator = &str` set the `type Generator` for `FromMirrordConfig`.

- On struct fields:
    - `nested` to use a value of a type that itself implement `MirrordConfig`. The value used is its `Generated` type.
    - `toggleable` wrap field with `ToggleableConfig` so will be allowed to pass bool instead of entire field
    - `env = &str` to load the value from the specified environment variable, if it's populated.
    - `default = &str` to set a default value to the field. This also implicitly `unwrap`s it.
    - `unstable` mark field as unstable and print an error message to the user.
    - `deprecated | deprecated = &str` mark field as deprecated and print either a default message or a custom one.
    - `rename = &str` pass `#[serde(rename = &str)]` to generated struct.

Example

```rust
/// MyConfig Comments
#[derive(MirrordConfig, Default)]
#[config(map_to = "MyFileConfig", derive = "Eq,PartialEq")] // If map_to isn't given, it will default to FileMyConfig
pub struct MyConfig {
    /// Value 1 Comment
    #[config(env = "VALUE_1")]
    pub value_1: Option<String>,

    /// Value 2 Comment
    #[config(env = "VALUE_2", default = "2")]
    pub value_2: i32,

    /// Value 3 Comment
    #[config(env = "VALUE_3", rename = "foobar")]
    pub value_3: String,

    /// Value 4 Comment
    #[config(nested)]
    pub value_4: OtherConfig,
}

```

The following struct will be generated:

```rust
/// MyConfig Comments
#[derive(Clone, Debug, Default, serde::Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MyFileConfig {
    /// Value 1 Comment
    pub value_1: Option<String>,

    /// Value 2 Comment
    pub value_2: Option<i32>,

    /// Value 3 Comment
    #[serde(rename = "foobar")]
    pub value_3: Option<String>,

    /// Value 4 Comment
    pub value_4: Option<<OtherConfig as FromMirrordConfig>::Generator>,
}

```
