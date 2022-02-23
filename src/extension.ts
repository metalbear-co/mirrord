import * as vscode from 'vscode';
import * as path from 'path';

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
export function activate(context: vscode.ExtensionContext) {

	let trackerDisposable = vscode.debug.registerDebugAdapterTrackerFactory('*', {
		createDebugAdapterTracker(_session: vscode.DebugSession) {
			return new ProcessCapturer();
		}
	});

	context.subscriptions.push(trackerDisposable);

	let debugDisposable = vscode.debug.onDidStartDebugSession(session => {
		const cp = require('child_process');

		// Get pods from kubectl and let user select one to mirror
		let pods = cp.execSync('kubectl get pods').toString().split('\n').slice(1, -1).map((line: string) => line.split(' ')[0]);

		vscode.window.showQuickPick(pods, { placeHolder: 'Select pod to mirror' }).then(pod => {
			// Find the debugged process' port
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
			// Infer container id from pod name
			let containerID = cp.execSync('kubectl get -o jsonpath="{.status.containerStatuses[*].containerID}" pod ' + pod);
			let filePath = context.asAbsolutePath(path.join('bintest.sh'));

			cp.exec('sh ' + filePath + ' ' + port + ' ' + containerID, (err: string, stdout: string, stderr: string) => {
				console.log('stdout: ' + stdout);
				console.log('stderr: ' + stderr);
				if (err) {
					console.log('error: ' + err);
				}
			});


		});
	});







	context.subscriptions.push(debugDisposable);
}

// this method is called when your extension is deactivated
export function deactivate() { }
