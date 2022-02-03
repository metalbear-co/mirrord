// The module 'vscode' contains the VS Code extensibility API
// Import the module and reference it with the alias vscode in your code below
import * as vscode from 'vscode';
import * as path from 'path';

class ProcessCapturer implements vscode.DebugAdapterTracker {
	static pid = 0;

	onDidSendMessage(m: any) {
		if (m.event === 'process' && m.body.systemProcessId) {
			ProcessCapturer.pid = m.body.systemProcessId;
		}
	}
}

// this method is called when your extension is activated
// your extension is activated the very first time the command is executed
export function activate(context: vscode.ExtensionContext) {

	vscode.debug.registerDebugAdapterTrackerFactory('*', {
		createDebugAdapterTracker(session: vscode.DebugSession) {
			return new ProcessCapturer();
		}
	});

	let filePath = context.asAbsolutePath(path.join('bintest.sh'));
	let arg = '';
	let disposable = vscode.debug.onDidStartDebugSession(session => {
		const cp = require('child_process');
		if (session.configuration.mirrord && session.configuration.mirrord.port) {
			arg = session.configuration.mirrord.port;
		} else {
			arg = ProcessCapturer.pid;
			// The below sometimes doesn't work (possibly because the process hasn't properly started yet)
			// let result = [];
			// try{
			// result = cp.execSync(`lsof -a -P -p ${ProcessCapturer.pid} -iTCP -sTCP:LISTEN -Fn`);
			// }
			// catch(e){
			// 	console.log(e);
			// }
			// port = result.toString('utf-8').split('\n').reverse()[1].split(':')[1];
		}

		cp.exec('sh ' + filePath + ' ' + arg, (err: string, stdout: string, stderr: string) => {
			console.log('stdout: ' + stdout);
			console.log('stderr: ' + stderr);
			if (err) {
				console.log('error: ' + err);
			}
		});
	});

	context.subscriptions.push(disposable);


}

// this method is called when your extension is deactivated
export function deactivate() { }
