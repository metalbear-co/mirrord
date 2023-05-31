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

1. `cd` into `mirrod/mirrod/config`;
2. run `medschool` (from wherever the tool is built, for example `./target/release/medschool`);
3. add the page header to the generated file (`title`, `date`, etc);
4. copy the new `configuration.md` to mirrord.dev `content/docs/overview`;
