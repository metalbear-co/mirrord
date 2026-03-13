import { rename } from "fs";

function applyRename(oldName, newName) {
  rename(oldName, newName, (err) => {
    if (err) {
      console.error(`issue3456 failed with ${err}`);
      throw err;
    }
  });
}

// The `rename` operation happens in the agent.
applyRename("/tmp/krakus_i.pol", "/tmp/krakus_ii.pol");

// The `rename` operation happens locally.
applyRename("/tmp/leszko_i.pol", "/tmp/leszko_ii.pol");

// The `rename` operation also happens locally, but we apply a mapping, since `/tmp/piast.pol`
// doesn't exist locally, we're mapping it to `/tmp/wanda.pol`.
applyRename("/tmp/piast.pol", "/tmp/lech.pol");

console.log("test issue 3456: SUCCESS");
