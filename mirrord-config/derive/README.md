# Mirrord Config Derive

This crate implements the derive macro `MirrordConfig`, which introduces the `config` attribute:

- On a struct:
    - `map_to = <Some Ident>` set the name of the new generated struct (defaults to `format!("Mapped{}", <struct name>)`)

- On struct fields:
    - `unwrap` to `unwrap` the `Option` and use its inner type as the field's value in the generated struct. 
    - `nested` to use a value of a type that itself implement `MirrordConfig`. The value used is its `Generated` type.
    - `env = &str` to load the value from the specified environment variable, if it's populated.
    - `default = &str` to set a default value to the field. This also implicitly `unwrap`s it.


Example

```rust
#[derive(MirrordConfig, Default)]
#[config(map_to = SomeConfig)] // If map_to isn't given, it will default to MappedMyConfig
pub struct MyConfig {
    #[config(env = "VALUE_1")]
    pub value_1: Option<String>,

    #[config(env = "VALUE_2", default = "2")]
    pub value_2: Option<i32>,

    #[config(env = "VALUE_3", unwrap)]
    pub value_3: Option<String>,

    #[config(nested)]
    pub value_4: OtherConfig,
}


```

The following struct will be generated:

```rust
#[derive(Clone, Debug)]
pub struct SomeConfig {
    pub value_1: Option<String>,
    pub value_2: i32,
    pub value_3: String,
    pub value_4: <OtherConfig as MirrordConfig>::Generated,
}

```


### Limitations:
* Value types must either be `Option<impl FromStr>` or implement MirrordConfig (in which case `nested` should be set to true)
