"""Tests `elf_closure.py` on small archives that the tests write."""

import io
import os
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

from python.runfiles import Runfiles


def elf_with_interpreter(interpreter):
    """Returns a 64-bit little-endian ELF file whose only content is its dynamic linker's path."""
    header_size = 64
    program_header_size = 56
    path = interpreter.encode() + b"\0"
    header = b"\x7fELF\x02\x01\x01".ljust(16, b"\0") + struct.pack(
        "<HHIQQQIHHHHHH",
        2,  # e_type: an executable
        62,  # e_machine: x86-64
        1,  # e_version
        0,  # e_entry
        header_size,  # e_phoff
        0,  # e_shoff
        0,  # e_flags
        header_size,  # e_ehsize
        program_header_size,  # e_phentsize
        1,  # e_phnum
        64,  # e_shentsize
        0,  # e_shnum
        0,  # e_shstrndx
    )
    # p_type (PT_INTERP), p_flags, p_offset, p_vaddr, p_paddr, p_filesz, p_memsz, p_align
    interpreter_header = struct.pack(
        "<IIQQQQQQ", 3, 4, header_size + program_header_size, 0, 0, len(path), len(path), 1
    )
    return header + interpreter_header + path


class ElfClosureTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def run_elf_closure(self, members, *paths):
        """Writes `members`, a list of `TarInfo`s and their contents, to an archive, runs the
        script on it with `paths`, and returns the output's members by name, with their contents."""
        input_path = self.root / "input.tar"
        with tarfile.open(input_path, "w") as archive:
            for member, contents in members:
                archive.addfile(member, io.BytesIO(contents) if contents is not None else None)

        runfiles = Runfiles.Create()
        assert runfiles is not None
        elf_closure = runfiles.Rlocation(os.environ["ELF_CLOSURE"])
        assert elf_closure is not None

        output_path = self.root / "output.tar.gz"
        subprocess.run([sys.executable, elf_closure, input_path, output_path, *paths], check=True)

        members = {}
        with tarfile.open(output_path) as output:
            for member in output.getmembers():
                file = output.extractfile(member)
                members[member.name] = (member, file.read() if file else None)
        return members

    def test_hard_link_is_a_file_with_its_targets_contents_and_libraries(self):
        def file(name, contents):
            member = tarfile.TarInfo(name)
            member.size = len(contents)
            member.mode = 0o755
            return member, contents

        program = elf_with_interpreter("/lib/ld.so")
        link = tarfile.TarInfo("usr/bin/alias")
        link.type = tarfile.LNKTYPE
        link.linkname = "usr/bin/tool"
        link.mode = 0o755

        output = self.run_elf_closure(
            [
                file("usr/bin/tool", program),
                (link, None),
                file("lib/ld.so", b"the dynamic linker"),
            ],
            "/usr/bin/alias",
        )

        alias, contents = output["./usr/bin/alias"]
        self.assertTrue(alias.isfile())
        self.assertEqual(contents, program)
        self.assertEqual(output["./lib/ld.so"][1], b"the dynamic linker")


if __name__ == "__main__":
    unittest.main()
