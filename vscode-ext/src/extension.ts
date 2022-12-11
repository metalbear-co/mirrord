import { CoreV1Api, V1NamespaceList, V1PodList } from '@kubernetes/client-node';
import { resolve } from 'path';
import * as fs from 'node:fs';
import * as vscode from 'vscode';
import YAML from 'yaml';
import * as child_process from "child_process";
const TOML = require('toml');
const semver = require('semver');
const https = require('https');
const path = require('path');
const os = require('os');
const glob = require('glob');

const LIBRARIES: { [platform: string]: [var_name: string, lib_name: string] } = {
	'darwin': ['DYLD_INSERT_LIBRARIES', 'libmirrord_layer.dylib'],
	'linux': ['LD_PRELOAD', 'libmirrord_layer.so']
};

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
	let state = context.globalState;
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
					vscode.window.showInformationMessage('Your version of mirrord is outdated, you should update.', 'Update', "Don't show again").then(item => {
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

	context.globalState.update('enabled', false);
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

/// Uses `mirrord ls` to get a list of all targets.
async function getMirrordTargets(cliPath: string, targetNamespace?: string) {
	let targets: string[] = [];
	const args = ['ls'];

	if (targetNamespace) {
		args.push('-n', targetNamespace);
	}

	const child = child_process.spawn(cliPath, args);

	child.stdout.on('data', (data: any) => {
		targets = JSON.parse(data.toString());
	});

	child.stderr.on('data', (data: any) => {
		throw new Error(data.toString());
	});

	await new Promise((resolve, reject) => {
		child.on('exit', resolve);
		child.on('error', reject);
	});

	return targets;
}


async function extractMirrordLayer() {
	let extensionPath = globalContext.extensionPath;
	let cliPath = path.join(extensionPath, 'mirrord');
	// spawn a process the creats a dylib in the extension folder
	const child = child_process.spawn(cliPath, ["extract", extensionPath]);
	
	await new Promise((resolve, reject) => {
		child.on('exit', resolve);
		child.on('error', reject);
	});
	
	if (!fs.existsSync(path.join(extensionPath, LIBRARIES[os.platform()]))) {		
		throw new Error('Failed to extract mirrord layer.');
	}
}

class ConfigurationProvider implements vscode.DebugConfigurationProvider {
	async resolveDebugConfiguration(folder: vscode.WorkspaceFolder | undefined, config: vscode.DebugConfiguration, token: vscode.CancellationToken): Promise<vscode.DebugConfiguration | null | undefined> {
		if (!globalContext.globalState.get('enabled')) {
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

		let libraryPath: string, cliPath: string;

		const mode = globalContext.extensionMode;
		const extensionPath = globalContext.extensionPath;

		if (mode === vscode.ExtensionMode.Development) {
			const debugPath = path.join(path.dirname(extensionPath), "target", "debug");
			libraryPath = debugPath;
			cliPath = path.join(debugPath, "mirrord");
		} else {
			await extractMirrordLayer();
			libraryPath = extensionPath;
			cliPath = path.join(extensionPath, "mirrord");
		}

		let [environmentVariableName, libraryName] = LIBRARIES[os.platform()];
		config.env ||= {};
		config.env[environmentVariableName] = path.join(libraryPath, libraryName);

		// If target wasn't specified in the config file, let user choose pod from dropdown			
		if (!await isTargetInFile()) {
			let targetNamespace = await parseNamespace();
			let targets = targetNamespace != undefined ? await getMirrordTargets(cliPath, targetNamespace) : await getMirrordTargets(cliPath);
			await vscode.window.showQuickPick(targets, { placeHolder: 'Select a target path to mirror' }).then(async targetName => {
				return new Promise(async resolve => {
					if (!targetName) {
						return;
					}
					config.env['MIRRORD_IMPERSONATED_TARGET'] = targetName;
					return resolve(config);
				});
			});
		}

		let filePath = await configFilePath();
		if (filePath) {
			config.env['MIRRORD_CONFIG_FILE'] = filePath;
		}

		if (config.type === "go") {
			config.env["MIRRORD_SKIP_PROCESSES"] = "dlv;debugserver;compile;go;asm;cgo;link";
			// use our custom delve to fix being loaded into debugserver
			if (os.platform() === "darwin") {
				config.dlvToolPath = path.join(globalContext.extensionPath, "dlv");
			}
		}

		return new Promise(resolve => {
			return resolve(config);
		});
	}
}

