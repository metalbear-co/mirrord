# medschool

> It's what makes docs MD.

- Tal

Generates a markdown file from a Rust crate's documentation.

`medschool` will extract the docs from the current directory, and build a nice
`configuration.md` file.

To use it simply run `medschool` on the directory you want to get docs from, for example:

```sh
cd rust-project

medschool --root-type DocsRootType
```

It'll look into `rust-project/src` and produce `rust-project/configuration.md`.

## Usage

To generate the `configuration.md` that you see in the 
[docs page](https://metalbear.co/mirrord/docs/reference/configuration/) we use the `medschool` tool as such:

```sh
bazel run //medschool -- \
    --root-type LayerConfig \
    --input "$PWD/mirrord/config/src" \
    --output "$PWD/.configuration.md"
```

`bazel run` runs `medschool` in its runfiles directory, so the paths must be absolute.

You can also use the `--prepend` argument to include a file at the start of the generated Markdown file.

```sh
bazel run //medschool -- \
    --root-type LayerConfig \
    --prepend "$PWD/header.txt" \
    --input "$PWD/mirrord/config/src" \
    --output "$PWD/.configuration.md"
```
