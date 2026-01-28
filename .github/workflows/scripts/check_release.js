/**
 * GitHub Actions script to check for release assets and Docker images.
 * Triggered via actions/github-script with script-path.
 * 
 * Expectations:
 * - process.env.CHECK_TYPE: 'mirrord' or 'operator'
 * - process.env.OPERATOR_MONITOR_TOKEN (required for operator checks)
 */

module.exports = async ({ github, context, core, exec }) => {
    const checkType = process.env.CHECK_TYPE;

    if (checkType === 'mirrord') {
        await checkMirrordRelease({ github, context, core, exec });
    } else if (checkType === 'operator') {
        await checkOperatorRelease({ github, context, core, exec });
    } else {
        throw new Error(`Unknown CHECK_TYPE: ${checkType}`);
    }
};

/**
 * Logic for checking Mirrord releases.
 */
async function checkMirrordRelease({ github, context, core, exec }) {
    const { owner, repo } = context.repo;

    // === REQUIRED ASSETS ===
    const requiredAssets = [
        "mirrord_linux_x86_64.zip",
        "mirrord_linux_x86_64.shasum256",
        "mirrord_linux_x86_64",
        "libmirrord_layer_linux_x86_64.so",
        "mirrord_linux_aarch64.zip",
        "mirrord_linux_aarch64.shasum256",
        "mirrord_linux_aarch64",
        "libmirrord_layer_linux_aarch64.so",
        "mirrord_mac_universal.zip",
        "mirrord_mac_universal.shasum256",
        "mirrord_mac_universal",
        "libmirrord_layer_mac_universal.dylib"
    ];

    // === OPTIONAL ASSETS ===
    const optionalAssets = [
        "mirrord.exe",
        "mirrord.exe.sha256",
        "mirrord_layer_win.dll",
        "mirrord_layer_win.dll.sha256"
    ];

    core.info(`Checking latest release for ${owner}/${repo}...`);

    let release;
    try {
        const res = await github.rest.repos.getLatestRelease({ owner, repo });
        release = res.data;
    } catch (error) {
        const msg = `Failed to get latest release: ${error.message}`;
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
        return;
    }

    const tag = release.tag_name;
    const htmlUrl = release.html_url;
    const assetNames = release.assets.map(a => a.name);
    core.info(`Latest release tag: ${tag}`);
    core.info(`Assets on release: ${assetNames.join(", ") || "(none)"}`);

    // === Check Mirrord Docker Image ===
    const mirrordImage = `ghcr.io/${owner}/${repo}:${tag}`;
    core.info(`Checking for Mirrord Docker image: ${mirrordImage}`);

    const missingRequired = requiredAssets.filter(name => !assetNames.includes(name));

    try {
        await exec.exec("docker", ["manifest", "inspect", mirrordImage]);
        core.info(`✅ Mirrord Docker image found: ${mirrordImage}`);
    } catch (error) {
        core.warning(`Failed to find Mirrord Docker image ${mirrordImage}`);
        missingRequired.push(`Docker Image (${mirrordImage})`);
    }

    const missingOptional = optionalAssets.filter(name => !assetNames.includes(name));

    core.setOutput("tag", tag);
    core.setOutput("html_url", htmlUrl);
    core.setOutput("missing_required", missingRequired.join(", "));
    core.setOutput("missing_optional", missingOptional.join(", "));

    if (missingRequired.length > 0) {
        const msg = `Missing REQUIRED assets on latest release ${tag}: ${missingRequired.join(", ")}`;
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
    } else {
        core.info("All required assets are present on latest release.");
        if (missingOptional.length > 0) {
            core.warning(`Missing OPTIONAL (Windows) assets on latest release ${tag}: ${missingOptional.join(", ")}`);
        }
        core.setOutput("ok", "true");
        core.setOutput("error", "");
    }
}

/**
 * Logic for checking Operator releases.
 */
async function checkOperatorRelease({ github, context, core, exec }) {
    const operatorRepo = "metalbear-co/operator";
    const token = process.env.OPERATOR_MONITOR_TOKEN;

    if (!token) {
        throw new Error("No OPERATOR_MONITOR_TOKEN available. This is required for checking private operator releases.");
    }

    core.info(`Fetching latest Operator version from GitHub API for ${operatorRepo}...`);
    let operatorTag;
    try {
        const { stdout } = await exec.getExecOutput("curl", [
            "-s",
            "-H", `Authorization: token ${token}`,
            `https://api.github.com/repos/${operatorRepo}/releases/latest`
        ]);

        const releaseData = JSON.parse(stdout);
        operatorTag = releaseData.tag_name;

        if (!operatorTag) {
            core.info(`API Response: ${stdout.substring(0, 1000)}`);
            throw new Error(releaseData.message || "No tag_name found in release data");
        }
        core.info(`Latest Operator release tag from GitHub: ${operatorTag}`);
    } catch (error) {
        const msg = `Failed to get latest operator version from GitHub: ${error.message}`;
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
        return;
    }

    core.setOutput("tag", operatorTag);

    const operatorImage = `ghcr.io/metalbear-co/operator:${operatorTag}`;
    core.info(`Checking for Operator Docker image: ${operatorImage}`);

    try {
        await exec.exec("docker", ["manifest", "inspect", operatorImage]);
        core.info(`✅ Operator Docker image found: ${operatorImage}`);
        core.setOutput("ok", "true");
        core.setOutput("error", "");
    } catch (error) {
        const msg = `Failed to find Operator Docker image ${operatorImage}`;
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
    }

    // === Check GitHub Release Page & Source Code Asset ===
    const releaseUrl = `https://github.com/${operatorRepo}/releases/tag/${operatorTag}`;
    core.info(`Checking Operator Release Page: ${releaseUrl}`);
    try {
        await exec.exec("curl", ["-fsI", releaseUrl]);
        core.info("✅ Operator Release Page is reachable.");
    } catch (error) {
        core.warning(`Operator Release Page unreachable (likely private): ${releaseUrl}`);
    }

    const assetUrl = `https://github.com/${operatorRepo}/archive/refs/tags/${operatorTag}.zip`;
    core.info(`Checking Operator Source Code Zip: ${assetUrl}`);
    try {
        await exec.exec("curl", [
            "-fsI",
            "-H", `Authorization: token ${token}`,
            assetUrl
        ]);
        core.info("✅ Operator Source Code Zip found.");
    } catch (error) {
        const msg = `Operator Source Code Zip unreachable (Auth failure?): ${assetUrl}`;
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
    }
}
