import * as fs from 'node:fs';
import * as vscode from 'vscode';
import YAML from 'yaml';
import { ChildProcessWithoutNullStreams, ExecException, spawn } from 'child_process';

const TOML = require('toml');
const semver = require('semver');
const https = require('https');
const path = require('path');
const os = require('os');
const glob = require('glob');
const util = require('node:util');
const exec = util.promisify(require('node:child_process').exec);



const CI_BUILD_PLUGIN = process.env.CI_BUILD_PLUGIN === 'true';
const DEFAULT_CONFIG = `
{
    "accept_invalid_certificates": false,
    "feature": {
        "network": {
            "incoming": "mirror",
            "outgoing": true
        },
        "fs": "read",
        "env": true
    }
}
`;

const TARGETLESS_TARGET_NAME = "No Target (\"targetless\")";

// Populate the path for the project's .mirrord folder
const MIRRORD_DIR = function () {
	let folders = vscode.workspace.workspaceFolders;
	if (folders === undefined) {
		throw new Error('No workspace folder found');
	}
	return vscode.Uri.joinPath(folders[0].uri, '.mirrord');
}();

const versionCheckEndpoint = 'https://version.mirrord.dev/get-latest-version';
const versionCheckInterval = 1000 * 60 * 3;

/// Key used to store the last selected target in the persistent state.
const LAST_TARGET_KEY = "mirrord-last-target";

let buttons: { toggle: vscode.StatusBarItem, settings: vscode.StatusBarItem };
let globalContext: vscode.ExtensionContext;

// Get the file path to the user's mirrord-config file, if it exists. 
async function configFilePath() {
	let fileUri = vscode.Uri.joinPath(MIRRORD_DIR, '?(*.)mirrord.+(toml|json|y?(a)ml)');
	let files = glob.sync(fileUri.fsPath);
	return files[0] || '';
}

// Display error message with help
function mirrordFailure() {
	vscode.window.showErrorMessage("mirrord failed. Please check the logs/errors.", "Get help on Discord", "Open an issue on GitHub", "Send us an email").then(value => {
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

}

function mirrordExecutionFromJson(data: string): MirrordExecution {
	let parsed = JSON.parse(data);
	return new MirrordExecution(parsed["environment"], parsed["patchedPath"]);
}

class MirrordAPI {
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
	spawn(args: string[]): ChildProcessWithoutNullStreams {
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

			mirrordFailure();

			if (match) {
				const error = JSON.parse(match)["message"];
				throw new Error(`mirrord failed to list available targets: ${error}`);
			} else {
				throw new Error(`mirrord failed to list available targets with an unknown error. Stderr: ${stderr}`)
			}
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
					reject("mirrord failed: timeout");
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
					const msg = `Failed to run mirrord: ${err}`;
					console.error(msg);
					mirrordFailure();
					reject(msg);
				});

				let stderrData = '';
				child.stderr.on('data', (data) => {
					stderrData += data.toString();
				});

				child.stderr.on('close', () => {
					const match = stderrData.match(/Error: (.*)/)?.[1];
					if (match) {
						const error = JSON.parse(match);
						mirrordFailure();
						vscode.window.showErrorMessage(`mirrord error: ${error["message"]}`).then(() => {
							vscode.window.showInformationMessage(error["help"]);
						});
						console.error(`mirrord error: ${error}`);
					} else if (stderrData) {
						mirrordFailure();
						vscode.window.showErrorMessage(`mirrord unknown error. Stderr: ${stderrData}`);
						console.error(`mirrord unknown error. Stderr: ${stderrData}`);
					}
				});


				let buffer = "";
				child.stdout.on('data', (data) => {
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
								resolve(mirrordExecutionFromJson(message["message"]));
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

async function openConfig() {
	let path = await configFilePath();
	if (!path) {
		path = vscode.Uri.joinPath(MIRRORD_DIR, 'mirrord.json');
		await vscode.workspace.fs.writeFile(path, Buffer.from(DEFAULT_CONFIG));
	}
	vscode.workspace.openTextDocument(path).then(doc => {
		vscode.window.showTextDocument(doc);
	});
}

async function toggle(context: vscode.ExtensionContext, button: vscode.StatusBarItem) {
	if (process.platform === "win32") {
		vscode.window.showErrorMessage('mirrord is not supported on Windows. You can use it via remote development or WSL.');
		return;
	}
	let state = context.workspaceState;
	if (state.get('enabled')) {
		state.update('enabled', false);
		button.text = 'Enable mirrord';
	} else {
		state.update('enabled', true);
		button.text = 'Disable mirrord';
	}
}

async function checkVersion(version: string) {
	let versionUrl = versionCheckEndpoint + '?source=1&version=' + version + '&platform=' + os.platform();
	https.get(versionUrl, (res: any) => {
		res.on('data', (d: any) => {
			const config = vscode.workspace.getConfiguration();
			if (config.get('mirrord.promptOutdated') !== false) {
				if (semver.lt(version, d.toString())) {
					vscode.window.showInformationMessage('New version of mirrord is available!', 'Update', "Don't show again").then(item => {
						if (item === 'Update') {
							vscode.env.openExternal(vscode.Uri.parse('vscode:extension/MetalBear.mirrord'));
						} else if (item === "Don't show again") {
							config.update('mirrord.promptOutdated', false);
						}
					});
				}
			}
		});

	}).on('error', (e: any) => {
		console.error(e);
	});
}

async function parseConfigFile() {
	let filePath: string = await configFilePath();
	if (filePath) {
		const file = (await vscode.workspace.fs.readFile(vscode.Uri.parse(filePath))).toString();
		if (filePath.endsWith('json')) {
			return JSON.parse(file);
		} else if (filePath.endsWith('yaml') || filePath.endsWith('yml')) {
			return YAML.parse(file);
		} else if (filePath.endsWith('toml')) {
			return TOML.parse(file);
		}
	}
}

async function isTargetInFile() {
	let parsed = await parseConfigFile();
	return (parsed && (typeof (parsed['target']) === 'string' || parsed['target']?.['path']));
}

// Gets namespace from config file
async function parseNamespace() {
	let parsed = await parseConfigFile();
	return parsed?.['target']?.['namespace'];
}


// this method is called when your extension is activated
// your extension is activated the very first time the command is executed
export async function activate(context: vscode.ExtensionContext) {
	globalContext = context;

	context.workspaceState.update('enabled', false);
	vscode.debug.registerDebugConfigurationProvider('*', new ConfigurationProvider(), 2);
	buttons = { toggle: vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0), settings: vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0) };

	const toggleCommandId = 'mirrord.toggleMirroring';
	context.subscriptions.push(vscode.commands.registerCommand(toggleCommandId, async function () {
		toggle(context, buttons.toggle);
	}));

	buttons.toggle.text = 'Enable mirrord';
	buttons.toggle.command = toggleCommandId;

	const settingsCommandId = 'mirrord.changeSettings';
	context.subscriptions.push(vscode.commands.registerCommand(settingsCommandId, openConfig));
	buttons.settings.text = '$(gear)';
	buttons.settings.command = settingsCommandId;

	for (const button of Object.values(buttons)) {
		context.subscriptions.push(button);
		button.show();
	};
}

// Get the name of the field that holds the exectuable in a debug configuration of the given type.
function getExecutableFieldName(config: vscode.DebugConfiguration): keyof vscode.DebugConfiguration {
	switch (config.type) {
		case "pwa-node":
		case "node": {
			return "runtimeExecutable";
		}
		case "python": {
			if ("python" in config) {
				return "python";
			}
			// Official documentation states the relevant field name is "python" (https://code.visualstudio.com/docs/python/debugging#_python), 
			// but when debugging we see the field is called "pythonPath".
			return "pythonPath";
		}
		default: {
			return "program";
		}
	}

}


class ConfigurationProvider implements vscode.DebugConfigurationProvider {
	async resolveDebugConfiguration(folder: vscode.WorkspaceFolder | undefined, config: vscode.DebugConfiguration, token: vscode.CancellationToken): Promise<vscode.DebugConfiguration | null | undefined> {

		if (!globalContext.workspaceState.get('enabled')) {
			return config;
		}

		// For some reason resolveDebugConfiguration runs twice for Node projects. __parentId is populated.
		if (config.__parentId || config.env?.["__MIRRORD_EXT_INJECTED"] === 'true') {
			return config;
		}

		// do not send telemetry for CI runs
		if (vscode.env.isTelemetryEnabled && !CI_BUILD_PLUGIN) {
			let lastChecked = globalContext.globalState.get('lastChecked', 0);
			if (lastChecked < Date.now() - versionCheckInterval) {
				checkVersion(globalContext.extension.packageJSON.version);
				globalContext.globalState.update('lastChecked', Date.now());
			}
		}

		let mirrordApi = new MirrordAPI(globalContext);

		config.env ||= {};
		let target = null;

		let configPath = await configFilePath();
		// If target wasn't specified in the config file, let user choose pod from dropdown
		if (!await isTargetInFile()) {
			let targets = await mirrordApi.listTargets(configPath);
			if (targets.length === 0) {
				vscode.window.showInformationMessage(
					"No mirrord target available in the configured namespace. " +
					"You can run targetless, or set a different target namespace or kubeconfig in the mirrord configuration file.",
				);
			}
			targets.push(TARGETLESS_TARGET_NAME);
			let targetName = await vscode.window.showQuickPick(targets, { placeHolder: 'Select a target path to mirror' });

			if (targetName) {
				if (targetName !== TARGETLESS_TARGET_NAME) {
					target = targetName;
				}
				globalContext.globalState.update(LAST_TARGET_KEY, targetName);
				globalContext.workspaceState.update(LAST_TARGET_KEY, targetName);
			}
			if (!target) {
				vscode.window.showInformationMessage("mirrord running targetless");
			}
		}

		if (config.type === "go") {
			config.env["MIRRORD_SKIP_PROCESSES"] = "dlv;debugserver;compile;go;asm;cgo;link;git;gcc";
			// use our custom delve to fix being loaded into debugserver

			if (process.platform === "darwin") {
				config.dlvToolPath = path.join(globalContext.extensionPath, "bin", "darwin", "dlv-" + process.arch);
			}
		} else if (config.type === "python") {
			config.env["MIRRORD_DETECT_DEBUGGER_PORT"] = "debugpy";
		}

		// Add a fixed range of ports that VS Code uses for debugging.
		// TODO: find a way to use MIRRORD_DETECT_DEBUGGER_PORT for other debuggers.
		config.env["MIRRORD_IGNORE_DEBUGGER_PORTS"] = "45000-65535";

		let executableFieldName = getExecutableFieldName(config);

		let executionInfo;
		if (executableFieldName in config) {
			executionInfo = await mirrordApi.binaryExecute(target, configPath, config[executableFieldName]);
		} else {
			// Even `program` is not in config, so no idea what's the executable in the end.
			executionInfo = await mirrordApi.binaryExecute(target, configPath, null);
		}

		// For sidestepping SIP on macOS. If we didn't patch, we don't change that config value.
		let patchedPath = executionInfo?.patchedPath;
		if (patchedPath) {
			config[executableFieldName] = patchedPath;
		}

		let env = executionInfo?.env;
		config.env = Object.assign({}, config.env, env);

		config.env["__MIRRORD_EXT_INJECTED"] = 'true';

		return config;
	}
}