import * as vscode from 'vscode';
import * as path from 'node:path';
import * as fs from 'node:fs';
import { ChildProcess, ExecException, spawn } from 'child_process';
import { globalContext } from './extension';
const util = require('node:util');
const exec = util.promisify(require('node:child_process').exec);

/// Key used to store the last selected target in the persistent state.
export const LAST_TARGET_KEY = "mirrord-last-target";

// Option in the target selector that represents no target.
export const TARGETLESS_TARGET_NAME = "No Target (\"targetless\")";

// Display error message with help
export function mirrordFailure(err: any) {
    vscode.window.showErrorMessage("mirrord failed to start. Please check the logs/errors.", "Get help on Discord", "Open an issue on GitHub", "Send us an email").then(value => {
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
        return new MirrordExecution(parsed["environment"], parsed["patchedPath"]);
    }

}

// API to interact with the mirrord CLI, runs in the "ext" mode
export class MirrordAPI {
    context: vscode.ExtensionContext;
    cliPath: string;

    constructor(context: vscode.ExtensionContext) {
        this.context = context;
        // for easier debugging, use the local mirrord cli if we're in development mode
        if (context.extensionMode === vscode.ExtensionMode.Development) {
            const universalApplePath = path.join(path.dirname(this.context.extensionPath), "target", "universal-apple-darwin", "debug", "mirrord");
            if (process.platform === "darwin" && fs.existsSync(universalApplePath)) {
                this.cliPath = universalApplePath;
            } else {
                const debugPath = path.join(path.dirname(this.context.extensionPath), "target", "debug");
                this.cliPath = path.join(debugPath, "mirrord");
            }
        } else {
            if (process.platform === "darwin") { // macos binary is universal for all architectures
                this.cliPath = path.join(context.extensionPath, 'bin', process.platform, 'mirrord');
            } else if (process.platform === "linux") {
                this.cliPath = path.join(context.extensionPath, 'bin', process.platform, process.arch, 'mirrord');
            } else if (vscode.extensions.getExtension('MetalBear.mirrord')?.extensionKind === vscode.ExtensionKind.Workspace) {
                throw new Error("Unsupported platform: " + process.platform + " " + process.arch);
            } else {
                console.log("Running in Windows.");
                this.cliPath = '';
                return;
            }
            fs.chmodSync(this.cliPath, 0o755);
        }
    }

    // Execute the mirrord cli with the given arguments, return stdout, stderr.
    async exec(args: string[]): Promise<[string, string]> {
        // Check if arg contains space, and if it does, wrap it in quotes
        args = args.map(arg => arg.includes(' ') ? `"${arg}"` : arg);
        let commandLine = [this.cliPath, ...args];
        // clone env vars and add MIRRORD_PROGRESS_MODE
        let env = {
            // eslint-disable-next-line @typescript-eslint/naming-convention
            "MIRRORD_PROGRESS_MODE": "json",
            ...process.env,
        };
        try {
            const value = await new Promise((resolve, reject) => {
                exec(commandLine.join(' '), { env }, (error: ExecException | null, stdout: string, stderr: string) => {
                    if (error) {
                        reject(error);
                    } else {
                        resolve([stdout, stderr]);
                    }
                });
            });

            return value as [string, string];
        } catch (error: any) {
            return ['', error.message];
        }
    }

    // Spawn the mirrord cli with the given arguments
    // used for reading/interacting while process still runs.
    spawn(args: string[]): ChildProcess {
        let env = {
            // eslint-disable-next-line @typescript-eslint/naming-convention
            "MIRRORD_PROGRESS_MODE": "json",
            ...process.env,
        };
        return spawn(this.cliPath, args, { env });
    }


    /// Uses `mirrord ls` to get a list of all targets.
    /// Targets are sorted, with an exception of the last used target being the first on the list.
    async listTargets(configPath: string | null | undefined): Promise<string[]> {
        const args = ['ls'];

        if (configPath) {
            args.push('-f', configPath);
        }

        const [stdout, stderr] = await this.exec(args);

        if (stderr) {
            const match = stderr.match(/Error: (.*)/)?.[1];
            const notificationError = match ? JSON.parse(match)["message"] : stderr;
            mirrordFailure(stderr);
            throw new Error(`mirrord failed to 'ls' targets\n${notificationError}`);
        }

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
    async binaryExecute(target: string | null, configFile: string | null, executable: string | null): Promise<MirrordExecution | null> {
        /// Create a promise that resolves when the mirrord process exits
        return await vscode.window.withProgress({
            location: vscode.ProgressLocation.Notification,
            title: "mirrord",
            cancellable: false
        }, (progress, _) => {
            return new Promise<MirrordExecution | null>((resolve, reject) => {
                setTimeout(() => {
                    reject("Timeout");
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
                let child = this.spawn(args);

                child.on('error', (err) => {
                    console.error('Failed to run mirrord.' + err);
                    mirrordFailure(err);
                    reject(err);
                });

                let stderrData = '';
                child.stderr?.on('data', (data) => {
                    stderrData += data.toString();
                });

                child.stderr?.on('close', () => {
                    const match = stderrData.match(/Error: (.*)/)?.[1];
                    if (match) {
                        const error = JSON.parse(match);
                        mirrordFailure(stderrData);
                        vscode.window.showErrorMessage(error["message"]).then(() => {
                            vscode.window.showInformationMessage(error["help"]);
                        });
                    } else {
                        mirrordFailure(stderrData);
                        vscode.window.showErrorMessage(stderrData);
                    }
                    console.error(`mirrord stderr: ${stderrData}`);
                });


                let buffer = "";
                child.stdout?.on('data', (data) => {
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
                                resolve(MirrordExecution.mirrordExecutionFromJson(message["message"]));
                                let res = JSON.parse(message["message"]);
                                resolve(res);
                            } else {
                                reject(null);
                            }
                            return;
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