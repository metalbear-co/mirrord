
const fs = require('fs');
const assert = require('assert');

const data = fs.readFileSync("/tmp/test_file.txt", {encoding: "utf-8"})
console.log(data)
assert.equal(data, "hello");