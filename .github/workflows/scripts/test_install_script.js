#!/usr/bin/env node

const { execSync } = require('child_process');
const fs = require('fs');

const INSTALL_URL = "https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh";
const LOG_FILE = "/tmp/install_sh.log";
const VERSION_LOG = "/tmp/mirrord_version_curl.log";

function appendToGithubOutput(key, value) {
    const outputPath = process.env.GITHUB_OUTPUT;
    if (outputPath) {
        fs.appendFileSync(outputPath, `${key}=${value}\n`);
    } else {
        console.log(`[OUTPUT] ${key}=${value}`);
    }
}

async function run() {
    console.log(`Installing mirrord via curl|bash from: ${INSTALL_URL}`);

    try {
        // Run the installer. We use bash explicitly to ensure the pipe works as expected.
        const installCmd = `curl -fsSL "${INSTALL_URL}" | bash >${LOG_FILE} 2>&1`;
        execSync(installCmd, { stdio: 'inherit', shell: '/bin/bash' });
    } catch (err) {
        console.error(`curl|bash installer failed.`);
        appendToGithubOutput("ok", "false");
        appendToGithubOutput("error", `curl|bash installer failed from ${INSTALL_URL}. See logs below.`);

        if (fs.existsSync(LOG_FILE)) {
            console.log("----- installer output -----");
            console.log(fs.readFileSync(LOG_FILE, 'utf8'));
        }
        process.exit(0);
    }

    // Verify mirrord is on PATH. We use a login shell to ensure PATH updates are picked up.
    try {
        execSync('command -v mirrord', { shell: '/bin/bash' });
    } catch (err) {
        console.error("mirrord is not on PATH after curl|bash install.");
        appendToGithubOutput("ok", "false");
        appendToGithubOutput("error", "mirrord is not on PATH after curl|bash install.");
        process.exit(0);
    }

    // Verify mirrord --version
    try {
        const versionCmd = `mirrord --version >${VERSION_LOG} 2>&1`;
        execSync(versionCmd, { shell: '/bin/bash' });
        const versionOutput = fs.readFileSync(VERSION_LOG, 'utf8').trim();
        console.log(`curl|bash installer succeeded. Version: ${versionOutput}`);

        appendToGithubOutput("ok", "true");
        appendToGithubOutput("error", "");
    } catch (err) {
        console.error("mirrord --version failed after curl|bash install.");
        appendToGithubOutput("ok", "false");
        appendToGithubOutput("error", "mirrord --version failed after curl|bash install.");
        if (fs.existsSync(VERSION_LOG)) {
            console.log("----- mirrord --version output (curl) -----");
            console.log(fs.readFileSync(VERSION_LOG, 'utf8'));
        }
        process.exit(0);
    }
}

run();
