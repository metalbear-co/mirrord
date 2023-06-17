import * as vscode from 'vscode';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { ChildProcessWithoutNullStreams, spawn } from 'child_process';
import { globalContext } from './extension';

/// Key used to store the last selected target in the persistent state.
export const LAST_TARGET_KEY = "mirrord-last-target";

// Option in the target selector that represents no target.
export const TARGETLESS_TARGET_NAME = "No Target (\"targetless\")";

// Display error message with help
export function mirrordFailure(error: string) {
    vscode.window.showErrorMessage(`${error}. Please check the logs/errors.`, "Get help on Discord", "Open an issue on GitHub", "Send us an email").then(value => {
        if (value === "Get help on Discord") {
            vscode.env.openExternal(vscode.Uri.parse('https://discord.gg/metalbear'));
        } else if (value === "Open an issue on GitHub") {
            vscode.env.openExternal(vscode.Uri.parse('https://github.com/metalbear-co/mirrord/issues/new/choose'));
        } else if (value === "Send us an email") {
            vscode.env.openExternal(vscode.Uri.parse('mailto:hi@metalbear.co'));
        }
    });
}

// Like the Rust MirrordExecution struct.
class MirrordExecution {

    env: Map<string, string>;
    patchedPath: string | null;

    constructor(env: Map<string, string>, patchedPath: string | null) {
        this.env = env;
        this.patchedPath = patchedPath;
    }

    static mirrordExecutionFromJson(data: string): MirrordExecution {
        const parsed = JSON.parse(data);
        return new MirrordExecution(parsed["environment"], parsed["patched_path"]);
    }

}

// API to interact with the mirrord CLI, runs in the "ext" mode
export class MirrordAPI {
    cliPath: string;

    constructor(cliPath: string) {
        this.cliPath = cliPath
    }

    // Return environment for the spawned mirrord cli processes.
    private static getEnv(): NodeJS.ProcessEnv {
        // clone env vars and add MIRRORD_PROGRESS_MODE
        return {
            // eslint-disable-next-line @typescript-eslint/naming-convention
            "MIRRORD_PROGRESS_MODE": "json",
            ...process.env,
        };
    }

    // Execute the mirrord cli with the given arguments, return stdout.
    private async exec(args: string[]): Promise<string> {
        const child = this.spawn(args);

        return await new Promise<string>((resolve, reject) => {
            let stdoutData = "";
            let stderrData = "";

            child.stdout.on("data", (data) => stdoutData += data.toString());
            child.stderr.on("data", (data) => stderrData += data.toString());

            child.on("error", (err) => {
                console.error(err);
                reject(`process failed: ${err.message}`);
            });

            child.on("close", (code, signal) => {
                const match = stderrData.match(/Error: (.*)/)?.[1];
                if (match) {
                    const error = JSON.parse(match);
                    vscode.window
                        .showErrorMessage(`mirrord error: ${error["message"]}`)
                        .then(() => {
                            if (error["help"]) {
                                vscode.window.showInformationMessage(error["help"]);
                            }
                        });
                    return reject(error["message"]);
                }

                if (code) {
                    return reject(`process exited with error code: ${code}`);
                }

                if (signal !== null) {
                    return reject(`process was killed by signal: ${signal}`);
                }

                resolve(stdoutData);
            });
        });
    }

    // Spawn the mirrord cli with the given arguments
    // used for reading/interacting while process still runs.
    private spawn(args: string[]): ChildProcessWithoutNullStreams {
        return spawn(this.cliPath, args, { env: MirrordAPI.getEnv() });
    }

    /**
     * Runs mirrord --version and returns the version string.
     */
    async getBinaryVersion(): Promise<string | undefined> {
        const stdout = await this.exec(["--version"]);
        // parse mirrord x.y.z
        return stdout.split(" ")[1].trim();
    }

    /// Uses `mirrord ls` to get a list of all targets.
    /// Targets are sorted, with an exception of the last used target being the first on the list.
    async listTargets(configPath: string | null | undefined): Promise<string[]> {
        const args = ['ls'];
        if (configPath) {
            args.push('-f', configPath);
        }

        const stdout = await this.exec(args);

        const targets: string[] = JSON.parse(stdout);
        targets.sort();

        let lastTarget: string | undefined = globalContext.workspaceState.get(LAST_TARGET_KEY)
            || globalContext.globalState.get(LAST_TARGET_KEY);

        if (lastTarget !== undefined) {
            const idx = targets.indexOf(lastTarget);
            if (idx !== -1) {
                targets.splice(idx, 1);
                targets.unshift(lastTarget);
            }
        }

        return targets;
    }

    // Run the extension execute sequence
    // Creating agent and gathering execution runtime (env vars to set)
    // Has 60 seconds timeout
    async binaryExecute(target: string | null, configFile: string | null, executable: string | null): Promise<MirrordExecution> {
        /// Create a promise that resolves when the mirrord process exits
        return await vscode.window.withProgress({
            location: vscode.ProgressLocation.Notification,
            title: "mirrord",
            cancellable: false
        }, (progress, _) => {
            return new Promise<MirrordExecution>((resolve, reject) => {
                setTimeout(() => {
                    reject("timeout");
                }, 60 * 1000);

                const args = ["ext"];
                if (target) {
                    args.push("-t", target);
                }
                if (configFile) {
                    args.push("-f", configFile);
                }
                if (executable) {
                    args.push("-e", executable);
                }

                const child = this.spawn(args);

                let stderrData = "";
                child.stderr.on("data", (data) => stderrData += data.toString());

                child.on("error", (err) => {
                    console.error(err);
                    reject(`process failed: ${err.message}`);
                });

                child.on("close", (code, signal) => {
                    const match = stderrData.match(/Error: (.*)/)?.[1];
                    if (match) {
                        const error = JSON.parse(match);
                        vscode.window
                            .showErrorMessage(`mirrord error: ${error["message"]}`)
                            .then(() => {
                                if (error["help"]) {
                                    vscode.window.showInformationMessage(error["help"]);
                                }
                            });
                        return reject(error["message"]);
                    }

                    if (code) {
                        return reject(`process exited with error code: ${code}`);
                    }

                    if (signal !== null) {
                        return reject(`process was killed by signal: ${signal}`);
                    }
                });

                let buffer = "";
                child.stdout.on("data", (data) => {
                    console.log(`mirrord: ${data}`);
                    buffer += data;
                    // fml - AH
                    let messages = buffer.split("\n");
                    for (const rawMessage of messages.slice(0, -1)) {
                        if (!rawMessage) {
                            break;
                        }
                        // remove from buffer + \n;
                        buffer = buffer.slice(rawMessage.length + 1);

                        let message;
                        try {
                            message = JSON.parse(rawMessage);
                        } catch (e) {
                            console.error("Failed to parse message from mirrord: " + data);
                            return;
                        }

                        // First make sure it's not last message
                        if ((message["name"] === "mirrord preparing to launch") && (message["type"]) === "FinishedTask") {
                            if (message["success"]) {
                                progress.report({ message: "mirrord started successfully, launching target." });
                                return resolve(MirrordExecution.mirrordExecutionFromJson(message["message"]));
                            }
                        }

                        if (message["type"] === "Warning") {
                            vscode.window.showWarningMessage(message["message"]);
                        } else {
                            // If it is not last message, it is progress
                            let formattedMessage = message["name"];
                            if (message["message"]) {
                                formattedMessage += ": " + message["message"];
                            }
                            progress.report({ message: formattedMessage });
                        }
                    }
                });
            });
        });
    }
}
