// The module 'vscode' contains the VS Code extensibility API
// Import the module and reference it with the alias vscode in your code below
import * as vscode from 'vscode';
import * as path from 'path';


// this method is called when your extension is activated
// your extension is activated the very first time the command is executed
export function activate(context: vscode.ExtensionContext) {

	// Use the console to output diagnostic information (console.log) and errors (console.error)
	// This line of code will only be executed once when your extension is activated
	console.log('Congratulations, your extension "mirrord" is now active!');

	let filePath = context.asAbsolutePath(path.join('bintest.sh'));

	let disposable = vscode.debug.onDidStartDebugSession(session => {
		if (session.configuration.mirrord) {
			const cp = require('child_process');
			cp.exec('sh ' + filePath + ' ' + session.configuration.mirrord.port, (err: string, stdout: string, stderr: string) => {
				console.log('stdout: ' + stdout);
				console.log('stderr: ' + stderr);
				if (err) {
					console.log('error: ' + err);
				}
			});
		}
	});

	context.subscriptions.push(disposable);


}

// this method is called when your extension is deactivated
export function deactivate() { }
