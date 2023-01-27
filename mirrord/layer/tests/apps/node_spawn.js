const { exec } = require('child_process');
const process = require("process");

// Echo is a built-in, so we will get a new process for the shell, but not a new one for echo.
// This starts: ["/bin/sh", "-c", "echo \"Hello over shell\""]
exec('echo "Hello over shell"', (err, stdout, stderr) => {
  if (err) {
    // node couldn't execute the command
    process.exit(1)
  }

  // the *entire* stdout and stderr (buffered)
  console.log(stdout);
  if (stderr) {
    console.error(stderr);
  }
});
