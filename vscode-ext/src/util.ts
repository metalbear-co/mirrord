import * as vscode from 'vscode';

/// Key used to store the last selected target in the persistent state.
export const LAST_TARGET_KEY = "mirrord-last-target";

/// Option in the target selector that represents no target.
export const TARGETLESS_TARGET_NAME = "No Target (\"targetless\")";

// Display error message with help
export function mirrordFailure(err: any) {
	vscode.window.showErrorMessage("mirrord failed to start. Please check the logs/errors.", "Get help on Discord", "Open an issue on GitHub", "Send us an email").then(value => {
		if (value === "Get help on Discord") {
			vscode.env.openExternal(vscode.Uri.parse('https://discord.gg/metalbear'));
		} else if (value === "Open an issue on GitHub") {
			vscode.env.openExternal(vscode.Uri.parse('https://github.com/metalbear-co/mirrord/issues/new/choose'));
		} else if (value === "Send us an email") {
			vscode.env.openExternal(vscode.Uri.parse('mailto:hi@metalbear.co'));
		}
	});
}