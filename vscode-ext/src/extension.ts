import * as fs from 'node:fs';
import * as vscode from 'vscode';
import YAML from 'yaml';
import { ChildProcess, spawn } from 'child_process';

const TOML = require('toml');
const semver = require('semver');
const https = require('https');
const path = require('path');
const os = require('os');
const glob = require('glob');
const util = require('node:util');
const exec = util.promisify(require('node:child_process').exec);



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

let buttons: { toggle: vscode.StatusBarItem, settings: vscode.StatusBarItem };
let globalContext: vscode.ExtensionContext;

// Get the file path to the user's mirrord-config file, if it exists. 
async function configFilePath() {
	let fileUri = vscode.Uri.joinPath(MIRRORD_DIR, '?(*.)mirrord.+(toml|json|y?(a)ml)');
	let files = glob.sync(fileUri.fsPath);
	return files[0] || '';
}

// Display error message with help
function mirrordFailure(err: any) {
	vscode.window.showErrorMessage("mirrord failed to start. Please check the logs/errors.", "Get help on Discord").then(value => {
		if (value === "Get help on Discord") {
			vscode.env.openExternal(vscode.Uri.parse('https://discord.gg/pSKEdmNZcK'));
		}
	});
}

class MirrordAPI {
	context: vscode.ExtensionContext;
	cliPath: string;

	constructor(context: vscode.ExtensionContext) {
		this.context = context;
		// for easier debugging, use the local mirrord cli if we're in development mode
		if (context.extensionMode === vscode.ExtensionMode.Development) {
			const debugPath = path.join(path.dirname(this.context.extensionPath), "target", "debug");
			this.cliPath = path.join(debugPath, "mirrord");
		} else {
			this.cliPath = path.join(context.extensionPath, 'mirrord');
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
		let value = await exec(commandLine.join(' '), { "env": env });
		return [value.stdout as string, value.stderr as string];
	}

	// Spawn the mirrord cli with the given arguments
	// used for reading/interacting while process still runs.
	spawn(args: string[]): ChildProcess {
		let env = {
			// eslint-disable-next-line @typescript-eslint/naming-convention
			"MIRRORD_PROGRESS_MODE": "json",
			...process.env,
		};
		return spawn(this.cliPath, args, { "env": env });
	}


	/// Uses `mirrord ls` to get a list of all targets.
	async listTargets(targetNamespace: string | null | undefined): Promise<[string]> {
		const args = ['ls'];

		if (targetNamespace) {
			args.push('-n', targetNamespace);
		}

		let [stdout, stderr] = await this.exec(args);

		if (stderr) {
			mirrordFailure(stderr);
			throw new Error("error occured listing targets: " + stderr);
		}

		return JSON.parse(stdout);
	}

	// Run the extension execute sequence
	// Creating agent and gathering execution runtime (env vars to set)
	// Has 60 seconds timeout
	async binaryExecute(target: string | null, configFile: string | null): Promise<Map<string, string> | null> {
		/// Create a promise that resolves when the mirrord process exits
		return await vscode.window.withProgress({
			location: vscode.ProgressLocation.Notification,
			title: "mirrord",
			cancellable: false
		}, (progress, _) => {
			return new Promise<Map<string, string> | null>((resolve, reject) => {
				setTimeout(() => { reject("Timeout"); }, 60 * 1000);

				const args = ["ext"];
				if (target) {
					args.push("-t", target);
				}
				if (configFile) {
					args.push("-f", configFile);
				}
				let child = this.spawn(args);

				child.on('error', (err) => {
					console.error('Failed to run mirrord.' + err);
					mirrordFailure(err);
					reject(err);
				});

				child.stderr?.on('data', (data) => {
					console.error(`mirrord stderr: ${data}`);
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
						}
						catch (e) {
							console.error("Failed to parse message from mirrord: " + data);
							return;
						}

						// First make sure it's not last message
						if ((message["name"] === "mirrord preparing to launch") && (message["type"]) === "FinishedTask") {
							if (message["success"]) {
								progress.report({message: "mirrord started successfully, launching target."});
								let res = JSON.parse(message["message"]);
								resolve(res["environment"]);
							} else {
								mirrordFailure(null);
								reject(null);
							}
							return;
						}
						// If it is not last message, it is progress
						let formattedMessage = message["name"];

						if (message["message"]) {
							formattedMessage += ": " + message["message"];
						}
						progress.report({message: formattedMessage});
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




class ConfigurationProvider implements vscode.DebugConfigurationProvider {
	async resolveDebugConfiguration(folder: vscode.WorkspaceFolder | undefined, config: vscode.DebugConfiguration, token: vscode.CancellationToken): Promise<vscode.DebugConfiguration | null | undefined> {
		if (!globalContext.workspaceState.get('enabled')) {
			return new Promise(resolve => { resolve(config); });
		}

		if (config.__parentId) { // For some reason resolveDebugConfiguration runs twice for Node projects. __parentId is populated.
			return new Promise(resolve => {
				return resolve(config);
			});
		}

		if (vscode.env.isTelemetryEnabled) {
			let lastChecked = globalContext.globalState.get('lastChecked', 0);
			if (lastChecked < Date.now() - versionCheckInterval) {
				checkVersion(globalContext.extension.packageJSON.version);
				globalContext.globalState.update('lastChecked', Date.now());
			}
		}


		const mode = globalContext.extensionMode;
		const extensionPath = globalContext.extensionPath;

		let mirrordApi = new MirrordAPI(globalContext);

		config.env ||= {};
		let target = null;

		// If target wasn't specified in the config file, let user choose pod from dropdown			
		if (!await isTargetInFile()) {
			let targetNamespace = await parseNamespace();
			let targets = await mirrordApi.listTargets(targetNamespace);
			await vscode.window.showQuickPick(targets, { placeHolder: 'Select a target path to mirror' }).then(async targetName => {
				return new Promise(async resolve => {
					if (!targetName) {
						return;
					}
					target = targetName;
					return resolve(config);
				});
			});
		}

		let configPath = await configFilePath();

		if (config.type === "go") {
			config.env["MIRRORD_SKIP_PROCESSES"] = "dlv;debugserver;compile;go;asm;cgo;link;git";
			// use our custom delve to fix being loaded into debugserver
			if (os.platform() === "darwin") {
				config.dlvToolPath = path.join(globalContext.extensionPath, "dlv");
			}
		}

		let env = await mirrordApi.binaryExecute(target, configPath);
		config.env = Object.assign({}, config.env, env);
		return new Promise(resolve => {
			return resolve(config);
		});
	}
}