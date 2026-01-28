/**
 * GitHub Actions script to check version.mirrord.dev vs download URLs.
 */
module.exports = async ({ github, context, core, exec }) => {
    const apiUrl = "https://version.mirrord.dev/v1/version";
    core.info(`Fetching version from: ${apiUrl}`);

    let version;
    try {
        const { stdout } = await exec.getExecOutput("curl", ["-fsSL", apiUrl]);
        // The endpoint returns plain text like: 3.174.0%
        version = stdout.trim().replace(/%$/, "").trim();
    } catch (error) {
        const msg = `Failed to fetch ${apiUrl}: ${error.message}`;
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
        return;
    }

    if (!version) {
        const msg = "Empty version string from version.mirrord.dev";
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
        return;
    }

    core.info(`Normalized version from API: '${version}'`);
    core.setOutput("version", version);

    const base = "https://github.com/metalbear-co/mirrord/releases/download";
    const urls = [
        `${base}/${version}/mirrord_linux_x86_64`,
        `${base}/${version}/mirrord_linux_aarch64`,
        `${base}/${version}/mirrord_mac_universal`
    ];

    core.info("Checking URLs:");
    for (const u of urls) {
        core.info(`  - ${u}`);
    }

    const missing = [];
    for (const u of urls) {
        try {
            // HEAD to avoid downloading full binaries
            await exec.exec("curl", ["-fsSIL", u]);
        } catch (error) {
            core.warning(`URL appears unreachable (HEAD failed): ${u}`);
            missing.push(u);
        }
    }

    if (missing.length > 0) {
        const msg = `Some download URLs are missing or not reachable for version ${version}`;
        core.warning(msg);
        core.setOutput("ok", "false");
        core.setOutput("error", msg);
        core.setOutput("missing_urls", missing.join(","));
    } else {
        core.info(`All download URLs for version ${version} are reachable.`);
        core.setOutput("ok", "true");
        core.setOutput("error", "");
        core.setOutput("missing_urls", "");
    }
};
