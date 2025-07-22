import { exec } from 'child_process';
import { exit } from "process";
import { promisify } from 'util';

const asyncExec = promisify(exec);

async function run() {
  try {
    const { stdout, stderr } = await asyncExec('cat /app/test.txt');

    console.log(stdout);
    if (stderr) {
      console.error(stderr);
    }

    console.log(">> nodejs finished running");
  } catch (fail) {
    console.error(`>> nodejs exec failed with ${fail}`);

    exit(1);
  }
}

await run();
