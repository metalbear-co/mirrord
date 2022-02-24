import * as vscode from 'vscode';
import * as net from 'net';
import { Log } from '@kubernetes/client-node';

class ProcessCapturer implements vscode.DebugAdapterTracker {
	static pid: number;

	onDidSendMessage(m: any) {
		if (m.event === 'process' && m.body.systemProcessId) {
			ProcessCapturer.pid = m.body.systemProcessId;
		}
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

	let debugDisposable = vscode.debug.onDidStartDebugSession(async session => {
		const cp = require('child_process');

		const k8s = require('@kubernetes/client-node');

		const kc = new k8s.KubeConfig();
		kc.loadFromDefault();
		const k8sApi = kc.makeApiClient(k8s.CoreV1Api);

		// Get pods from kubectl and let user select one to mirror
		let pods = await k8sApi.listNamespacedPod('default');
		let podNames = pods.body.items.map((pod: { metadata: { name: any; }; }) => { return pod.metadata.name; });

		vscode.window.showQuickPick(podNames, { placeHolder: 'Select pod to mirror' }).then(async pod => {
			// Find the debugged process' port

			// Infer container id from pod name
			let containerID = cp.execSync('kubectl get -o jsonpath="{.status.containerStatuses[*].containerID}" pod ' + pod);

			// Infert port from process ID
			let port: string;
			if (session.configuration.mirrord && session.configuration.mirrord.port) {
				port = session.configuration.mirrord.port;
			} else {
				port = ProcessCapturer.pid.toString();
				let result = [];
				try {
					result = cp.execSync(`lsof -a -P -p ${ProcessCapturer.pid} -iTCP -sTCP:LISTEN -Fn`);
					port = result.toString('utf-8').split('\n').reverse()[1].split(':')[1];
				}
				catch (e) {
					console.log(e);
				}
			}


			let agentPod = {
				metadata: { name: 'agentpod' },
				spec: {
					hostPID: true,
					hostIPC: true,
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
								port
							]
						}
					]
				}

			};

			await k8sApi.createNamespacedPod('default', agentPod);

			const net = require('net');
			const stream = require('stream');
			let log = new k8s.Log(kc);
			let logStream = new stream.PassThrough();
			let connections: { [connection_id: string]: net.Socket; } = {};

			logStream.on('data', (chunk: Buffer) => {
				chunk.toString().split('\n').forEach((line: string) => {
					if (line) {
						let parsedLine = JSON.parse(line);
						let connectionId = parsedLine.content.connection_id;
						if (parsedLine['type'] === 'Connected') {
							let socket = new net.Socket();
							socket.connect(port, 'localhost');
							connections[connectionId] = socket;
						}
						else {
							let socket = connections[connectionId];
							if (parsedLine['type'] === 'Data') {
								socket.write(Buffer.from(parsedLine.content.data, 'base64'));
							}
							if (parsedLine['type'] === 'TCPEnded') {
								socket.end();
								delete connections[connectionId];
							}
						}
					}
				});
			});
			await log.log('default', 'agentpod', '', logStream, (err: any) => { console.log(err); }, { follow: true, tailLines: 50, pretty: false, timestamps: false });
		});

	});







	context.subscriptions.push(debugDisposable);
}

// this method is called when your extension is deactivated
export function deactivate() { }
