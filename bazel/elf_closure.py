"""Copies files out of a root file system in a tar archive, with the libraries that they load.

Images use it to keep their size down. Rather than ship Debian packages whole, an image takes only
the programs that it runs and the libraries that they load. The agent's image, for example, takes
`iptables` and `ss`, and the libraries that they and the agent load.

The size of those programs and libraries in the x86-64 agent image, with each approach:

| Approach                                                          | Raw     | Compressed |
| ----------------------------------------------------------------- | ------- | ---------- |
| This script                                                       | 10.7 MB | 4.0 MB     |
| `ldd` into a `FROM scratch` image (`ci-agent-runtime`, Debian 12) | 11.0 MB | 4.3 MB     |
| `debian:trixie-slim` with `iptables`, `iproute2` and `conntrack`  | 92.7 MB | 33.6 MB    |

It reads the ELF headers rather than running `ldd`, because `ldd` runs the program's dynamic linker,
which only works for the build machine's CPU, and the agent's image is built for both arm64 and
x86-64.
"""

import argparse
import gzip
import io
import posixpath
import struct
import tarfile

DT_NEEDED = 1
SHT_DYNAMIC = 6
PT_INTERP = 3


def elf_dependencies(data):
    """Returns the dynamic linker and the needed libraries of a 64-bit little-endian ELF file.

    Returns `None` for any other file, like a script.
    """
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        return None

    (phoff, shoff) = struct.unpack_from("<QQ", data, 0x20)
    (phentsize, phnum, shentsize, shnum) = struct.unpack_from("<HHHH", data, 0x36)

    interpreter = None
    for index in range(phnum):
        (p_type, _, p_offset, _, _, p_filesz) = struct.unpack_from("<IIQQQQ", data, phoff + index * phentsize)
        if p_type == PT_INTERP:
            interpreter = data[p_offset : p_offset + p_filesz].rstrip(b"\0").decode()

    def section(index):
        # sh_name, sh_type, sh_flags, sh_addr, sh_offset, sh_size, sh_link
        return struct.unpack_from("<IIQQQQI", data, shoff + index * shentsize)

    needed = []
    for index in range(shnum):
        (_, sh_type, _, _, offset, size, link) = section(index)
        if sh_type != SHT_DYNAMIC:
            continue
        strtab_offset = section(link)[4]
        for entry in range(offset, offset + size, 16):
            (tag, value) = struct.unpack_from("<qQ", data, entry)
            if tag == DT_NEEDED:
                start = strtab_offset + value
                needed.append(data[start : data.index(b"\0", start)].decode())

    return interpreter, needed


class Archive:
    """The members of a tar archive of a root file system, by their absolute paths."""

    def __init__(self, path):
        # Not a `with` block, because `read` extracts members after `__init__` returns.
        self.tar = tarfile.open(path)  # noqa: SIM115
        self.members = {}
        for member in self.tar.getmembers():
            self.members[posixpath.normpath(posixpath.join("/", member.name))] = member

    def resolve(self, path, links):
        """Returns `path` with every symbolic link in it followed, and adds each link to `links`."""
        parts = [part for part in path.split("/") if part]
        resolved = "/"
        followed = 0
        while parts:
            part = parts.pop(0)
            if part == ".":
                continue
            if part == "..":
                resolved = posixpath.dirname(resolved)
                continue

            candidate = posixpath.join(resolved, part)
            member = self.members.get(candidate)
            if member is None or not member.issym():
                resolved = candidate
                continue

            followed += 1
            if followed > 40:
                raise ValueError(f"too many symbolic links in {path}")
            links.add(candidate)
            if member.linkname.startswith("/"):
                resolved = "/"
            parts = [part for part in member.linkname.split("/") if part] + parts
        return resolved

    def read(self, path):
        """Returns the contents of the regular file at `path`, or of a hard link's target."""
        file = self.tar.extractfile(self.members[path])
        assert file is not None
        return file.read()


def main():
    parser = argparse.ArgumentParser(
        description="Copies files out of a root file system in a tar archive, with the libraries that they load.",
    )
    parser.add_argument("input", metavar="<input.tar>", help="the archive of the root file system")
    parser.add_argument("output", metavar="<output.tar.gz>", help="the archive to write")
    parser.add_argument(
        "--elf",
        action="append",
        default=[],
        metavar="<file>",
        help="an ELF file outside of the archive, whose libraries the output holds too",
    )
    parser.add_argument(
        "paths",
        nargs="+",
        metavar="<path>",
        help="a file or a directory in the archive, like /usr/sbin/iptables-nft",
    )
    # `parse_args` would reject an `--elf` between two paths.
    arguments = parser.parse_intermixed_args()
    archive = Archive(arguments.input)

    # The dynamic linker's search path on Debian: the multiarch directories, `/lib` and `/usr/lib`.
    library_directories = sorted(
        path
        for path, member in archive.members.items()
        if member.isdir()
        and (path in ("/lib", "/usr/lib") or posixpath.dirname(path) in ("/lib", "/usr/lib"))
        and path.endswith("-linux-gnu")
    ) + ["/lib", "/usr/lib"]

    kept = set()
    pending = []

    def keep(path):
        resolved = archive.resolve(path, kept)
        if resolved not in archive.members:
            raise FileNotFoundError(f"{path} isn't in the archive")
        if resolved in kept:
            return
        kept.add(resolved)
        member = archive.members[resolved]
        if member.isdir():
            prefix = resolved + "/"
            for name in archive.members:
                if name.startswith(prefix):
                    keep(name)
        elif member.isfile() or member.islnk():
            pending.append(archive.read(resolved))

    def keep_library(name):
        for directory in library_directories:
            candidate = posixpath.join(directory, name)
            try:
                resolved = archive.resolve(candidate, set())
            except ValueError:
                continue
            if resolved in archive.members:
                keep(candidate)
                return
        raise FileNotFoundError(f"no library {name} in {', '.join(library_directories)}")

    for path in arguments.paths:
        keep(path)
    for path in arguments.elf:
        with open(path, "rb") as file:
            pending.append(file.read())

    while pending:
        dependencies = elf_dependencies(pending.pop())
        if dependencies is None:
            continue
        interpreter, needed = dependencies
        if interpreter:
            keep(interpreter)
        for name in needed:
            keep_library(name)

    # Keeps the parent directories too, so that the layer creates them with their original modes.
    for path in list(kept):
        directory = posixpath.dirname(path)
        while directory != "/":
            if directory in archive.members:
                kept.add(directory)
            directory = posixpath.dirname(directory)

    # The gzip header leaves out the name and time, so the same files give the same layer digest.
    with (
        open(arguments.output, "wb") as file,
        gzip.GzipFile(filename="", fileobj=file, mode="wb", mtime=0) as compressed,
        tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as output,
    ):
        for path in sorted(kept):
            member = archive.members[path]
            copy = tarfile.TarInfo("." + path)
            copy.type = member.type
            copy.mode = member.mode
            copy.uid = copy.gid = 0
            copy.mtime = 0
            copy.linkname = member.linkname
            if member.isfile():
                copy.size = member.size
                output.addfile(copy, archive.tar.extractfile(member))
            elif member.islnk():
                # The output may not have the hard link's target, so the link becomes a file with
                # the target's contents.
                contents = archive.read(path)
                copy.type = tarfile.REGTYPE
                copy.linkname = ""
                copy.size = len(contents)
                output.addfile(copy, io.BytesIO(contents))
            else:
                output.addfile(copy)


if __name__ == "__main__":
    main()
