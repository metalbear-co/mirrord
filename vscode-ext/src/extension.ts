import { CoreV1Api, V1NamespaceList, V1PodList } from '@kubernetes/client-node';
import * as vscode from 'vscode';

const semver = require('semver');
const https = require('https');
const k8s = require('@kubernetes/client-node');
const path = require('path');
const os = require('os');

const LIBRARIES: { [platform: string]: [var_name: string, lib_name: string] } = {
	'darwin': ['DYLD_INSERT_LIBRARIES', 'libmirrord_layer.dylib'],
	'linux': ['LD_PRELOAD', 'libmirrord_layer.so']
};
const versionCheckEndpoint = 'https://version.mirrord.dev/get-latest-version';
const versionCheckInterval = 1000 * 60 * 3;

let buttons: { toggle: vscode.StatusBarItem, settings: vscode.StatusBarItem };
let globalContext: vscode.ExtensionContext;

function getK8sApi(): CoreV1Api {
	let k8sConfig = new k8s.KubeConfig();
	k8sConfig.loadFromDefault();
	return k8sConfig.makeApiClient(k8s.CoreV1Api);
}

async function changeSettings() {
	let agentNamespace = globalContext.workspaceState.get<string>('agentNamespace', 'default');
	let impersonatedPodNamespace = globalContext.workspaceState.get<string>('impersonatedPodNamespace', 'default');
	let fileOps = globalContext.workspaceState.get<boolean>('fileOps', true);
	let invalidCertificates = globalContext.workspaceState.get<boolean>('invalidCertificates', false);
	let trafficStealing = globalContext.workspaceState.get<boolean>('trafficStealing', false);
	let remoteDNS = globalContext.workspaceState.get<boolean>('remoteDNS', true);
	let outgoingTraffic = globalContext.workspaceState.get<boolean>('outgoingTraffic', true);
	let includeEnvironmentVariables = globalContext.workspaceState.get<string>('includeEnvironmentVariables', '*');
	let excludeEnvironmentVariables = globalContext.workspaceState.get<string>('excludeEnvironmentVariables', '');

	const options = ['Change namespace for mirrord agent (current: ' + agentNamespace + ')',
	'Change namespace for impersonated pod (current: ' + impersonatedPodNamespace + ')',
	'Toggle file operations (current: ' + (fileOps ? 'enabled' : 'disabled') + ')',
	'Toggle invalid certificates (current: ' + (invalidCertificates ? 'enabled' : 'disabled') + ')',
	'Toggle traffic stealing (current: ' + (trafficStealing ? 'enabled' : 'disabled') + ')',
	'Toggle remote DNS (current: ' + (remoteDNS ? 'enabled' : 'disabled') + ')',
	'Toggle outgoing traffic (current: ' + (outgoingTraffic ? 'enabled' : 'disabled') + ')',
	'Include environment variables (current: ' + includeEnvironmentVariables + ')',
	'Exclude environment variables (current: ' + excludeEnvironmentVariables + ')'];
	vscode.window.showQuickPick(options).then(async setting => {
		if (setting === undefined) {
			return;
		}
		else if (setting.startsWith('Toggle file')) {
			globalContext.workspaceState.update('fileOps', !fileOps);
		}
		else if (setting.startsWith('Toggle invalid certificates')) {
			globalContext.workspaceState.update('invalidCertificates', !invalidCertificates);
		} 
		else if (setting.startsWith('Toggle traffic stealing')) {
			globalContext.workspaceState.update('trafficStealing', !trafficStealing);
		}
		else if (setting.startsWith('Toggle remote DNS')) {
			globalContext.workspaceState.update('remoteDNS', !remoteDNS);
		}
		else if (setting.startsWith('Toggle outgoing traffic')) {
			globalContext.workspaceState.update('outgoingTraffic', !outgoingTraffic);
		}
		else if (setting.startsWith('Include environment variables')) {
			vscode.window.showInputBox({ prompt: 'Enter environment variables to include (separated by commas, * for all)', value: includeEnvironmentVariables }).then(async value => {
				if (value === undefined) {
					return;
				}
				globalContext.workspaceState.update('includeEnvironmentVariables', value);
			});
		}
		else if (setting.startsWith('Exclude environment variables')) {
			vscode.window.showInputBox({ prompt: 'Enter environment variables to exclude', value: excludeEnvironmentVariables }).then(async value => {
				if (value === undefined) {
					return;
				}
				globalContext.workspaceState.update('excludeEnvironmentVariables', value);
			});
		} else if (setting.startsWith('Change namespace')) {
			const namespaces: {
				response: any;
				body: V1NamespaceList;
			} = await getK8sApi().listNamespace();
			const namespaceNames = namespaces.body.items.map(namespace => namespace.metadata!.name!);
			vscode.window.showQuickPick(namespaceNames, { placeHolder: 'Select namespace' }).then(async namespaceName => {
				if (namespaceName === undefined) {
					return;
				}
				if (setting.startsWith('Change namespace for mirrord agent')) {
					globalContext.workspaceState.update('agentNamespace', namespaceName);
				} else if (setting.startsWith('Change namespace for impersonated pod')) {
					globalContext.workspaceState.update('impersonatedPodNamespace', namespaceName);
				}
			});
		}
	});
}


async function toggle(context: vscode.ExtensionContext, button: vscode.StatusBarItem) {
	let state = context.globalState;
	if (state.get('enabled')) {
		// vscode.debug.registerDebugConfigurationProvider('*', new ConfigurationProvider(), 2);
		state.update('enabled', false);
		button.text = 'Enable mirrord';
	} else {
		let lastChecked = state.get('lastChecked', 0);
		if (lastChecked < Date.now() - versionCheckInterval) {
			checkVersion(context.extension.packageJSON.version);
			state.update('lastChecked', Date.now());
		}
		state.update('enabled', true);
		button.text = 'Disable mirrord';
	}
}

async function checkVersion(version: string) {
	let versionUrl = versionCheckEndpoint + '?source=1&version=' + version;
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
	context.subscriptions.push(vscode.commands.registerCommand(settingsCommandId, changeSettings));
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

		const podNamespace = globalContext.workspaceState.get<string>('impersonatedPodNamespace', 'default');
		let k8sApi = getK8sApi();
		// Get pods from kubectl and let user select one to mirror
		let pods: { response: any, body: V1PodList } = await k8sApi.listNamespacedPod(podNamespace);
		let podNames = pods.body.items.map((pod) => pod.metadata!.name!);

		await vscode.window.showQuickPick(podNames, { placeHolder: 'Select pod to mirror' }).then(async podName => {
			return new Promise(resolve => {
				console.log(config);
				// Get pods from kubectl and let user select one to mirror
				if (k8sApi === null) {
					return;
				}

				let libraryPath;
				if (globalContext.extensionMode === vscode.ExtensionMode.Development) {
					libraryPath = path.join(path.dirname(globalContext.extensionPath), "target", "debug");
				} else {
					libraryPath = globalContext.extensionPath;
				}
				let [environmentVariableName, libraryName] = LIBRARIES[os.platform()];
				globalContext.workspaceState.update('impersonatedPodName', podName);
				config.env = {
					...config.env, ...{
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_AGENT_IMPERSONATED_POD_NAME': podName,
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_AGENT_IMPERSONATED_POD_NAMESPACE': podNamespace,
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_AGENT_NAMESPACE': globalContext.workspaceState.get('agentNamespace', 'default'),
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_FILE_OPS': globalContext.workspaceState.get('fileOps', true).toString(),
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_FILE_RO_OPS': 'false', // TODO: Add this to settings
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_ACCEPT_INVALID_CERTIFICATES': globalContext.workspaceState.get('invalidCertificates', false).toString(),
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_AGENT_TCP_STEAL_TRAFFIC': globalContext.workspaceState.get('trafficStealing', false).toString(),
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_REMOTE_DNS': globalContext.workspaceState.get('remoteDNS', true).toString(),
						// eslint-disable-next-line @typescript-eslint/naming-convention
						'MIRRORD_TCP_OUTGOING': globalContext.workspaceState.get('outgoingTraffic', true).toString(),
					}
				};

				// mirrord doesn't support specifying both the include and exclude env vars (even if they're empty)
				let include, exclude;
				if (include = globalContext.workspaceState.get('includeEnvironmentVariables')){
					config.env['MIRRORD_OVERRIDE_ENV_VARS_INCLUDE'] = include;
				}
				if (exclude = globalContext.workspaceState.get('excludeEnvironmentVariables')){
					config.env['MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE'] = exclude;
				}

				config.env[environmentVariableName] = path.join(libraryPath, libraryName);

				if(config.type === "go"){
					config.env["MIRRORD_SKIP_PROCESSES"] = "dlv;debugserver;compile;go;asm;cgo;link";
				}
				
				return resolve(config);
			});
		});

		// let user select container name if there are multiple containers in the pod
		const podName = globalContext.workspaceState.get('impersonatedPodName');
		const pod = pods.body.items.find(p => p.metadata!.name === podName!);
		const containerNames = pod!.spec!.containers.map(c => c.name!);
		if (containerNames.length > 1) {
			return await vscode.window.showQuickPick(containerNames, { placeHolder: 'Select containerName' }).then(async containerName => {
				return new Promise(resolve => {
					globalContext.workspaceState.update('impersonatedContainerName', containerName);
					config.env = {
						...config.env, ...{
							// eslint-disable-next-line @typescript-eslint/naming-convention
							'MIRRORD_IMPERSONATED_CONTAINER_NAME': containerName,
						}
					};
					return resolve(config);
				});
			});
		} else {
			return new Promise(resolve => {
				return resolve(config);
			});
		}
	}
}

