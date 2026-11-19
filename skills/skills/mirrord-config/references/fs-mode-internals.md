# fs.mode internals

Detail supporting the `fs.mode` guidance in the skill body.

## What is always read locally

A built-in local-by-default list applies in **every** fs mode, including the default `read`
(`mirrord/layer-lib/src/file/unix/read_local_by_default.rs`):

- **The process's current working directory** — your entire project tree
- **The executable being run**
- Runtime and package-manager paths: `/node_modules`, `/package.json`, `.yarnrc*`, `.tool-versions`
- Source and build artifacts by extension: `.js`, `.py`, `.pyc`, `.rb`, `.jar`, `.class`, `.so`, `.dll`, `.pdb`
- System paths: `/usr`, `/lib`, `/bin`, `/etc`, `/home`, `/opt`, `/tmp`, `/proc`, `/sys`, `/dev`
- Hidden files under `$HOME`

So `ts-node`, `nodemon`, `python -m`, `go run`, `dotnet watch` etc. all load local source under
the default config — no `fs` setting is needed for that.

## What `localwithoverrides` reads remotely

`localwithoverrides` reads `/etc/resolv.conf`, `/etc/hosts` and `/etc/hostname` remotely
by default ([`read_remote_by_default.rs`](https://github.com/metalbear-co/mirrord/blob/989aa3bb76682f8d7869752ea33fb50af8dd14f7/mirrord/layer-lib/src/file/unix/read_remote_by_default.rs#L3-L16)) — plus whatever you add to `fs.read_only` /
`fs.read_write`.
