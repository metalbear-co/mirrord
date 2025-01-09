import fs from 'fs';

fs.open("/app/file.local", (fail, fd) => {
  console.log(`open file.local ${fd}`);
  if (fd) {
    console.log(`SUCCESS /app/file.local ${fd}`);
  }

  if (fail) {
    console.error(`FAIL /app/file.local ${fail}`);
  }
});

fs.open("/app/file.not-found", (fail, fd) => {
  console.log(`open file.not-found ${fd}`);
  if (fd) {
    console.log(`SUCCESS /app/file.not-found ${fd}`);
  }

  if (fail) {
    console.error(`FAIL /app/file.not-found ${fail}`);
  }
});

fs.open("/app/file.read-only", (fail, fd) => {
  if (fd) {
    console.log(`SUCCESS /app/file.read-only ${fd}`);
  }

  if (fail) {
    console.error(`FAIL /app/file.read-only ${fail}`);
  }
});

fs.open("/app/file.read-only", "r+", (fail, fd) => {
  if (fd) {
    console.log(`SUCCESS r+ /app/file.read-only ${fd}`);
  }

  if (fail) {
    console.error(`FAIL r+ /app/file.read-only ${fail}`);
  }
});

fs.open("/app/file.read-write", "r+", (fail, fd) => {
  if (fd) {
    console.log(`SUCCESS /app/file.read-write ${fd}`);
  }

  if (fail) {
    console.error(`FAIL /app/file.read-write ${fail}`);
  }
});

