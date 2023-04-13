import os
import json

def main():
	info = os.statvfs("/tmp/test_file.txt")

	print(json.dumps(dict(
		f_bsize=info.f_bsize,
		f_frsize=info.f_frsize,
		f_blocks=info.f_blocks,
		f_bfree=info.f_bfree,
		f_bavail=info.f_bavail,
		f_files=info.f_files,
		f_ffree=info.f_ffree,
		f_favail=info.f_favail
	)))


if __name__ == '__main__':
    main()