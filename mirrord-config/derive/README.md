# Mirrord Config Derive

This crate implements derive macro `MirrordConfig`

it addes `config` attribute

for struct:
- `map_to = Ident` set the name of the struct generated (defaults to `format!("Mapped{}", <struct name>)`)

for fields:
- `unwrap = true` for generated struct for the field remove the Option and set it to inner type 
- `nested = true` for using a struct implementing `MirrordConfig` and insert it's `Generated` type
- `env = &str` load value from enviroment variable
- `default = &str` load value from provided string and also implicitly addes `unwrap = true` to the field


Example

```rust
#[derive(MirrordConfig, Default)]
#[config(map_to = MappedConfig)] // This is the default unless you want change it don't add this attribute
pub struct Config {
    #[config(env = "VALUE_1")]
    pub value_1: Option<String>,

    #[config(env = "VALUE_2", default = "2")]
    pub value_2: Option<i32>,

    #[config(env = "VALUE_3", unwrap = true)]
    pub value_3: Option<String>,

    #[config(nested = true)]
    pub value_4: OtherConfig,
}


```

this will generate another struct named MappedConfig

```rust
#[derive(Clone, Debug)]
pub struct MappedConfig {
    pub value_1: Option<String>,
    pub value_2: i32,
    pub value_3: String,
    pub value_4: <OtherConfig as MirrordConfig>::Generated,
}

```


### Limitations:
* values must be `Option<impl FromStr>` or a nested config (value implementing MirrordConfig)
* `unwrap` and `nested` properties are ignorant of the value theire existance only is required but due to current implementation value must be filled with something so recommned using `true`

