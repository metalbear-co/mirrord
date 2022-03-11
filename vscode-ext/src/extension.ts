import * as vscode from 'vscode';
import { MirrorD, MirrorEvent, K8SAPI } from '@metalbear/mirrord-core';

class ProcessCapturer implements vscode.DebugAdapterTracker {
	static pid: number;

	onDidSendMessage(m: any) {
		if (m.event === 'process' && m.body.systemProcessId) {
			ProcessCapturer.pid = m.body.systemProcessId;
		}
	}
}

let running = false;
let buttons: { toggle: vscode.StatusBarItem, settings: vscode.StatusBarItem };
let globalContext: vscode.ExtensionContext;

let openedPods = new Map<string, MirrorD>();
let k8sApi: K8SAPI | null = null;



async function cleanup(sessionId?: string, hideButtons: boolean = true) {
	vscode.commands.executeCommand('setContext', 'mirrord.activated', false);
	if (sessionId) {
		const mirror = openedPods.get(sessionId);
		await mirror?.stop();
	}
	else {
		for (const mirror of openedPods.values()) {
			try { await mirror.stop(); }
			catch (e) { console.error(e); }
		}
	}
	if (hideButtons) {
		for (const button of Object.values(buttons)) {
			button.hide();
		}
	}
	buttons.toggle.text = "Start mirrord";
	running = false;
}

async function runMirrorD() {
	let session = vscode.debug.activeDebugSession;
	if (session === undefined) {
		return;
	}
	if (k8sApi === null) {
		return;
	}

	if (running) {
		cleanup(session.id, false);
	} else {
		running = true;
		buttons.toggle.text = 'Stop mirrord (loading)';

		const namespace = globalContext.workspaceState.get<string>('namespace', 'default');
		// Get pods from kubectl and let user select one to mirror
		let pods = await k8sApi.listPods(namespace);
		let podNames = pods.body.items.map((pod: { metadata: { name: any; }; }) => { return pod.metadata.name; });

		vscode.window.showQuickPick(podNames, { placeHolder: 'Select pod to mirror' }).then(async podName => {
			if (session === undefined || podName === undefined || k8sApi === null) {
				return;
			}
			// Infer container id from pod name
			let selectedPod = pods.body.items.find((pod: { metadata: { name: any; }; }) => pod.metadata.name === podName);
			let containerID = selectedPod.status.containerStatuses[0].containerID.split('//')[1];
			let nodeName = selectedPod.spec.nodeName;

			// Infer port from process ID
			let port: string = '';
			if (session.configuration.mirrord && session.configuration.mirrord.port) {
				port = session.configuration.mirrord.port;
			} else {
				var netstat = require('node-netstat');
				netstat.commands['darwin'].args.push('-a'); // The default args don't list LISTEN ports on OSX
				// TODO: Check on other linux, windows
				netstat({
					filter: {
						pid: ProcessCapturer.pid,
						protocol: 'tcp',
					},
					sync: true,
					limit: 5,
				}, (data: { state: string; local: { port: string; }; }) => {
					if (data.state === 'LISTEN') {
						port = data.local.port;
					}
				});
			}

			if (!port) {
				throw new Error("Could not find the debugged process' port");
			}

			let packetCount = 0;


			function updateCallback(event: MirrorEvent, data: any) {
				switch (event) {
					case MirrorEvent.packetReceived:
						packetCount++;
						buttons.toggle.text = 'Stop mirrord (packets mirrored: ' + packetCount + ')';
						break;
				}
			}

			if (k8sApi === null) {
				return;
			}
			let remotePort = globalContext.workspaceState.get<number>('remotePort', 80);
			let mirrord = new MirrorD(nodeName, containerID, [{ remotePort: remotePort, localPort: parseInt(port) }], namespace, k8sApi, updateCallback);
			openedPods.set(session.id, mirrord);
			await mirrord.start();
			buttons.toggle.text = 'Stop mirrord';
		});
	}
};

async function changeSettings() {
	let namespace = globalContext.workspaceState.get<string>('namespace', 'default');
	let remotePort = globalContext.workspaceState.get<number>('remotePort', 80);

	const options = ['Change namespace (current: ' + namespace + ')', 'Change remote port (current: ' + remotePort + ')'];
	vscode.window.showQuickPick(options).then(async setting => {
		if (setting === undefined) {
			return;
		}

		if (setting.startsWith('Change namespace')) {
			let namespaces = await k8sApi?.api.listNamespace();
			let namespaceNames = namespaces.body.items.map((namespace: { metadata: { name: any; }; }) => { return namespace.metadata.name; });
			vscode.window.showQuickPick(namespaceNames, { placeHolder: 'Select namespace' }).then(async namespaceName => {
				if (namespaceName === undefined) {
					return;
				}
				globalContext.workspaceState.update('namespace', namespaceName);
			});

		} else if (setting.startsWith('Change remote port')) {
			vscode.window.showInputBox({ prompt: 'Enter new remote port' }).then(async port => {
				if (port === undefined) {
					return;
				}
				globalContext.workspaceState.update('remotePort', parseInt(port));
			});
		}
	});
}

// this method is called when your extension is activated
// your extension is activated the very first time the command is executed
export async function activate(context: vscode.ExtensionContext) {
	globalContext = context;
	k8sApi = new K8SAPI();
	openedPods = new Map();
	let trackerDisposable = vscode.debug.registerDebugAdapterTrackerFactory('*', {
		createDebugAdapterTracker(_session: vscode.DebugSession) {
			return new ProcessCapturer();
		}
	});
	context.subscriptions.push(trackerDisposable);

	buttons = {toggle: vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 1), settings: vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0)};
	
	const toggleCommandId = 'mirrord.toggleMirroring';
	context.subscriptions.push(vscode.commands.registerCommand(toggleCommandId, runMirrorD));
	buttons.toggle.text = 'Start mirrord';
	buttons.toggle.command = toggleCommandId;

	const settingsCommandId = 'mirrord.changeSettings';
	context.subscriptions.push(vscode.commands.registerCommand(settingsCommandId, changeSettings));
	buttons.settings.text = '$(gear)';
	buttons.settings.command = settingsCommandId;

	for (const button of Object.values(buttons)) {
		context.subscriptions.push(button);
	};


	let debugDisposable = vscode.debug.onDidStartDebugSession(async _session => {
		for (const button of Object.values(buttons)) {
			button.show();
		}
	});
	context.subscriptions.push(debugDisposable);

	let debugEndedDisposable = vscode.debug.onDidTerminateDebugSession(async (session: vscode.DebugSession) => {
		cleanup(session.id);

	});

	context.subscriptions.push(debugEndedDisposable);
	vscode.commands.executeCommand('setContext', 'mirrord.activated', true);

}

// this method is called when your extension is deactivated
export function deactivate() {
	cleanup();
}
