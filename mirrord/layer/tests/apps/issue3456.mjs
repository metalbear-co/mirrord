import { rename } from "fs";

rename("/tmp/krakus_i.pol", "/tmp/krakus_ii.pol", (err) => {
  if (err) {
    console.error(`issue3456 failed with ${fail}`);
    throw err;
  }

  console.log("test issue 3456: SUCCESS");
});
