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

		vscode.window.showQuickPick(podNames, { placeHolder: 'Select pod to mirror' }).then(async podName => {
			// Infer container id from pod name
			let containerID = pods.body.items.find((pod: { metadata: { name: any; }; }) => pod.metadata.name === podName)
				.status.containerStatuses[0].containerID;

			// Infert port from process ID
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

			const shortid = require('shortid');
			const agentPodName = 'mirrord-' + shortid.generate().toLowerCase();
			let agentPod = {
				metadata: { name: agentPodName },
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
								'abc',
								"--ports",
								port.toString()
							]
						}
					]
				}

			};

			try {
				await k8sApi.createNamespacedPod('default', agentPod);
			} catch (e) {
				console.log(e);
			}
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
			await log.log('default', agentPodName, '', logStream, (err: any) => { console.log(err); }, { follow: true, tailLines: 50, pretty: false, timestamps: false });
		});

	});







	context.subscriptions.push(debugDisposable);
}

// this method is called when your extension is deactivated
export function deactivate() { }
