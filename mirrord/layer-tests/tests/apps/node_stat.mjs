import { closeSync, fstatSync, lstatSync, openSync, statSync } from "fs";

const file = statSync("/app/file");
if (!file.isFile() || file.size !== 5) {
  throw new Error(`statSync: isFile ${file.isFile()}, size ${file.size}`);
}

const link = lstatSync("/app/link");
if (!link.isSymbolicLink()) {
  throw new Error("lstatSync: not a symlink");
}

const fd = openSync("/app/file");
const opened = fstatSync(fd);
if (!opened.isFile() || opened.size !== 5) {
  throw new Error(`fstatSync: isFile ${opened.isFile()}, size ${opened.size}`);
}
closeSync(fd);
