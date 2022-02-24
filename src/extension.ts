import * as vscode from 'vscode';
import * as net from 'net';
import * as request from 'request';
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
let logRequest: request.Request;
const openedPods: { [session_id: string]: string; } = {};
const k8s = require('@kubernetes/client-node');
const kc = new k8s.KubeConfig();
kc.loadFromDefault();
const k8sApi = kc.makeApiClient(k8s.CoreV1Api);

async function cleanup(sessionId?: string) {
	if (sessionId) {
		if (sessionId in openedPods) {
			let podName = openedPods[sessionId];
			delete openedPods[sessionId];
			try {
				await k8sApi.deleteNamespacedPod(podName, 'default');
			} catch (e) {
				console.log(e);
			}
		}
	}
	else {
		Object.values(openedPods).forEach(async (podName: string) => {
			try {
				await k8sApi.deleteNamespacedPod(podName, 'default');
			} catch (e) {
				console.log(e);
			}
		});
		statusBarButton.hide();
	}
	statusBarButton.text = "Start mirrord";
	running = false;
	if (logRequest) {
		logRequest.abort();
	}
}

// this method is called when your extension is activated
// your extension is activated the very first time the command is executed
export async function activate(context: vscode.ExtensionContext) {
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

	async function runMirrorD() {
		let session = vscode.debug.activeDebugSession;
		if (session === undefined) {
			return;
		}

		if (running) {
			cleanup(session.id);
		} else {
			running = true;
			statusBarButton.text = 'Stop mirrord (loading)';

			// Get pods from kubectl and let user select one to mirror
			let pods = await k8sApi.listNamespacedPod('default');
			let podNames = pods.body.items.map((pod: { metadata: { name: any; }; }) => { return pod.metadata.name; });

			vscode.window.showQuickPick(podNames, { placeHolder: 'Select pod to mirror' }).then(async podName => {
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

				const shortid = require('short-uuid');
				const agentPodName = 'mirrord-' + shortid.generate().toLowerCase();
				let agentPod = {
					metadata: { name: agentPodName },
					spec: {
						hostPID: true,
						nodeName: nodeName,
						restartPolicy: 'Never',
						volumes: [
							{
								name: 'containerd',
								hostPath: { path: '/run/containerd/containerd.sock' }
							}
						],
						containers: [
							{
								name: 'mirrord-agent',
								image: 'ghcr.io/metalbear-co/mirrord-agent:main',
								imagePullPolicy: 'Always',
								securityContext: { privileged: true },
								volumeMounts: [
									{
										mountPath: '/run/containerd/containerd.sock',
										name: 'containerd'
									}
								],
								command: [
									"./mirrord-agent",
									"--container-id",
									containerID,
									"--ports",
									'80'
								]
							}
						]
					}

				};

				try {
					await k8sApi.createNamespacedPod('default', agentPod);
					openedPods[session.id] = agentPodName;
				} catch (e) {
					console.log(e);
				}
				const net = require('net');
				const stream = require('stream');
				let log = new k8s.Log(kc);
				let logStream = new stream.PassThrough();
				let connections: { [connection_id: string]: net.Socket; } = {};
				let packetCount = 0;
				function updatePacketCount() {
					packetCount++;
					statusBarButton.text = 'Stop mirrord (packets mirrored: ' + packetCount + ')';
				}
				logStream.on('data', (chunk: Buffer) => {
					chunk.toString().split('\n').forEach((line: string) => {
						if (line) {
							let parsedLine = JSON.parse(line);
							let connectionId = parsedLine.content.connection_id;
							if (parsedLine['type'] === 'Error') {
								console.log(parsedLine.content.msg);
							}
							if (parsedLine['type'] === 'Connected') {
								let socket = new net.Socket();
								socket.connect(port, 'localhost');
								connections[connectionId] = socket;
								updatePacketCount();
							}
							else {
								let socket = connections[connectionId];
								if (parsedLine['type'] === 'Data') {
									socket.write(Buffer.from(parsedLine.content.data, 'base64'));
									updatePacketCount();
								}
								if (parsedLine['type'] === 'TCPEnded') {
									socket.end();
									delete connections[connectionId];
									updatePacketCount();
								}
							}
						}
					});
				});
				let waitForPod = true;
				let attempts = 0;
				while (waitForPod && attempts < 100) { //TODO: Must be a good way to do this
					let status = await k8sApi.readNamespacedPodStatus(agentPodName, 'default');
					if (status.body.status.phase === "Running") {
						waitForPod = false;
					}
					attempts += 1;
				}
				logRequest = await log.log('default', agentPodName, '', logStream, (err: any) => {
					console.log(err);
				}, { follow: true, tailLines: 50, pretty: false, timestamps: false });
				statusBarButton.text = 'Stop mirrord';
			});
		}
	};

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
