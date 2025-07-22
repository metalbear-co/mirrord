import fs from 'fs';

function test_open(path, mode) {
  fs.open(path, mode, (fail, fd) => {
    if (fd) {
      console.log(`SUCCESS ${mode} ${path} ${fd}`);
    }

    if (fail) {
      console.log(`FAIL ${mode} ${path} ${fail}`);
    }
  });
}

test_open("/app/file.local", "r");
test_open("/app/file.not-found", "r");
test_open("/app/file.read-only", "r");
test_open("/app/file.read-only", "r+");
test_open("/app/file.read-write", "r+");
