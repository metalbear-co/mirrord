import { CoreV1Api, V1NamespaceList, V1PodList } from '@kubernetes/client-node';
import { resolve } from 'path';
import * as vscode from 'vscode';
import YAML from 'yaml';
const TOML = require('toml');
const semver = require('semver');
const https = require('https');
const k8s = require('@kubernetes/client-node');
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

function getK8sApi(): CoreV1Api {
	let k8sConfig = new k8s.KubeConfig();
	k8sConfig.loadFromDefault();
	return k8sConfig.makeApiClient(k8s.CoreV1Api);
}

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

// Gets namespace from config file
async function parseNamespace() {
	let parsed = await parseConfigFile();
	return parsed?.['target']?.['namespace'] || 'default';
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

		let libraryPath: string;
		if (globalContext.extensionMode === vscode.ExtensionMode.Development) {
			libraryPath = path.join(path.dirname(globalContext.extensionPath), "target", "debug");
		} else {
			libraryPath = globalContext.extensionPath;
		}
		let [environmentVariableName, libraryName] = LIBRARIES[os.platform()];
		config.env[environmentVariableName] = path.join(libraryPath, libraryName);

		if (!await isTargetInFile()) { // If target wasn't specified in the config file, let user choose pod from dropdown
			let podNamespace: string;
			try {
				podNamespace = await parseNamespace();
			} catch (e: any) {
				vscode.window.showErrorMessage('Failed to parse mirrord config file', e.message);
				return;
			}
			let k8sApi = getK8sApi();
			// Get pods from kubectl and let user select one to mirror
			let pods: { response: any, body: V1PodList } = await k8sApi.listNamespacedPod(podNamespace);
			let podNames = pods.body.items.map((pod) => pod.metadata!.name!);
			let impersonatedPodName = '';
			await vscode.window.showQuickPick(podNames, { placeHolder: 'Select pod to mirror' }).then(async podName => {
				return new Promise(async resolve => {
					if (!k8sApi || !podName) {
						return;
					}
					impersonatedPodName = podName;
					config.env['MIRRORD_IMPERSONATED_TARGET'] = 'pod/' + impersonatedPodName;
					config.env['MIRRORD_TARGET_NAMESPACE'] = podNamespace; // This is unnecessary 99% of the time, but it ensures the namespace is the one the pod was selected from
					return resolve(config);
				});
			});

			// let user select container name if there are multiple containers in the pod		
			const pod = pods.body.items.find(p => p.metadata!.name === impersonatedPodName!);
			const containerNames = pod!.spec!.containers.map(c => c.name!);
			if (containerNames.length > 1) {
				await vscode.window.showQuickPick(containerNames, { placeHolder: 'Select container' }).then(async containerName => {
					return new Promise(resolve => {
						config.env['MIRRORD_IMPERSONATED_CONTAINER_NAME'] = containerName;
						return resolve(config);
					});
				});
			}
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

