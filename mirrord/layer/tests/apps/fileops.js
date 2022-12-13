const fs = require('fs');

const data = fs.readFileSync("/tmp/test_file.txt")
console.log(data)
assert(data === "hello");