import * as vscode from 'vscode';
import * as net from 'net';
import * as request from 'request';
import {PortSpec, MirrorD, MirrorEvent, K8SAPI} from '@mirrord/core';

class ProcessCapturer implements vscode.DebugAdapterTracker {
	static pid: number;

	onDidSendMessage(m: any) {
		if (m.event === 'process' && m.body.systemProcessId) {
			ProcessCapturer.pid = m.body.systemProcessId;
		}
	}
}

let running = false;
let statusBarButton: vscode.StatusBarItem;

let openedPods = new Map<string, MirrorD>();
let k8sApi: K8SAPI | null = null;



async function cleanup(sessionId?: string, hideButton: boolean = true) {
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
	if (hideButton) {
		statusBarButton.hide();
	}
	statusBarButton.text = "Start mirrord";
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
		statusBarButton.text = 'Stop mirrord (loading)';

		// Get pods from kubectl and let user select one to mirror
		let pods = await k8sApi.listPods('default');
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
					case MirrorEvent.PacketReceived:
						packetCount++;
						statusBarButton.text = 'Stop mirrord (packets mirrored: ' + packetCount + ')';
						break;
				}
			}

			if (k8sApi === null) {
				return;
			}
			let mirrord = new MirrorD(nodeName, containerID, [{remotePort: 80, localPort: parseInt(port)}], "default", k8sApi, updateCallback);
			await mirrord.start();
			statusBarButton.text = 'Stop mirrord';
		});
	}
};

// this method is called when your extension is activated
// your extension is activated the very first time the command is executed
export async function activate(context: vscode.ExtensionContext) {
	k8sApi = new K8SAPI();
	openedPods = new Map();
	let trackerDisposable = vscode.debug.registerDebugAdapterTrackerFactory('*', {
		createDebugAdapterTracker(_session: vscode.DebugSession) {
			return new ProcessCapturer();
		}
	});
	context.subscriptions.push(trackerDisposable);

	const commandId = 'mirrord.toggleMirroring';
	context.subscriptions.push(vscode.commands.registerCommand(commandId, runMirrorD));

	statusBarButton = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left);
	statusBarButton.text = 'Start mirrord';
	statusBarButton.command = commandId;
	context.subscriptions.push(statusBarButton);

	let debugDisposable = vscode.debug.onDidStartDebugSession(async session => {
		statusBarButton.show();
	});
	context.subscriptions.push(debugDisposable);

	let debugEndedDisposable = vscode.debug.onDidTerminateDebugSession(async (session: vscode.DebugSession) => {
		cleanup(session.id);

	});

	context.subscriptions.push(debugEndedDisposable);
}

// this method is called when your extension is deactivated
export function deactivate() {
	cleanup();
}
