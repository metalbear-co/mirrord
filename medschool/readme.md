# medschool

> It's what makes docs MD.

- Tal

Generates a markdown file from a Rust crate's documentation.

`medschool` will extract the docs from the current directory, and build a nice
`configuration.md` file.

To use it simply run `medschool` on the directory you want to get docs from, for example:

```sh
cd rust-project

medschool
```

It'll look into `rust-project/src` and produce `rust-project/configuration.md`.

## Usage

To generate the `configuration.md` that you see in the 
[docs page](https://mirrord.dev/docs/overview/configuration/) we use the `medschool` tool as such:

```sh
cargo run -p medschool -- --input ./mirrord/config/src --output ./configuration.md
```

You can also use the `--prepend` arg to include a file at the start of the generated markdown file. 

```sh
cargo run -p medschool -- --prepend ./header.txt --input ./mirrord/config/src --output [path to mirrord.dev docs page]
```
