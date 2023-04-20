import { exec } from 'child_process';
import { exit } from "process";
import { promisify } from 'util';

const asyncExec = promisify(exec);

// Promise of a sleep.
//
// - `time`: time to sleep for in ms.
function delay(time) {
  return new Promise(resolve => setTimeout(resolve, time));
}

async function run() {
  try {
    // Echo is a built-in, so we will get a new process for the shell, but not a new one for echo.
    // This starts: ["/bin/sh", "-c", "echo \"Hello over shell\""]
    const { stdout, stderr } = await asyncExec('echo "Hello over shell"');

    console.log(stdout);
    if (stderr) {
      console.error(stderr);
    }

    console.log(">> nodejs finished running bash");

    // Wait a little bit for the layer connection to close properly.
    await delay(5000);

    console.log(">> nodejs finished running");
  } catch (fail) {
    console.error(`>> nodejs exec failed with ${fail}`);
    exit(1);
  }
}

await run();
