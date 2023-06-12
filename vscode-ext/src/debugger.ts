import * as vscode from 'vscode';
import * as path from 'node:path';
import { globalContext } from './extension';
import { configFilePath, isTargetInFile } from './config';
import { LAST_TARGET_KEY, MirrordAPI, TARGETLESS_TARGET_NAME } from './api';
import { updateTelemetries } from './versionCheck';

const CI_BUILD_PLUGIN = process.env.CI_BUILD_PLUGIN === 'true';

/// Get the name of the field that holds the exectuable in a debug configuration of the given type.
function getExecutableFieldName(config: vscode.DebugConfiguration): keyof vscode.DebugConfiguration {
	switch (config.type) {
		case "pwa-node":
		case "node": {
			return "runtimeExecutable";
		}
		case "python": {
			if ("python" in config) {
				return "python";
			}
			// Official documentation states the relevant field name is "python" (https://code.visualstudio.com/docs/python/debugging#_python), 
			// but when debugging we see the field is called "pythonPath".
			return "pythonPath";
		}
		default: {
			return "program";
		}
	}

}


export class ConfigurationProvider implements vscode.DebugConfigurationProvider {
	async resolveDebugConfiguration(folder: vscode.WorkspaceFolder | undefined, config: vscode.DebugConfiguration, token: vscode.CancellationToken): Promise<vscode.DebugConfiguration | null | undefined> {

		if (!globalContext.workspaceState.get('enabled')) {
			return config;
		}

		// For some reason resolveDebugConfiguration runs twice for Node projects. __parentId is populated.
		if (config.__parentId || config.env?.["__MIRRORD_EXT_INJECTED"] === 'true') {
			return config;
		}

		updateTelemetries();

		let mirrordApi = new MirrordAPI(globalContext);

		config.env ||= {};
		let target = null;

		let configPath = await configFilePath();
		// If target wasn't specified in the config file, let user choose pod from dropdown
		if (!await isTargetInFile()) {
			let targets = await mirrordApi.listTargets(configPath);
			if (targets.length === 0) {
				vscode.window.showInformationMessage(
					"No mirrord target available in the configured namespace. " +
					"You can run targetless, or set a different target namespace or kubeconfig in the mirrord configuration file.",
				);
			}
			targets.push(TARGETLESS_TARGET_NAME);
			let targetName = await vscode.window.showQuickPick(targets, { placeHolder: 'Select a target path to mirror' });

			if (targetName) {
				if (targetName !== TARGETLESS_TARGET_NAME) {
					target = targetName;
				}
				globalContext.globalState.update(LAST_TARGET_KEY, targetName);
				globalContext.workspaceState.update(LAST_TARGET_KEY, targetName);
			}
			if (!target) {
				vscode.window.showInformationMessage("mirrord running targetless");
			}
		}

		if (config.type === "go") {
			config.env["MIRRORD_SKIP_PROCESSES"] = "dlv;debugserver;compile;go;asm;cgo;link;git;gcc";
			// use our custom delve to fix being loaded into debugserver

			if (process.platform === "darwin") {
				config.dlvToolPath = path.join(globalContext.extensionPath, "bin", "darwin", "dlv-" + process.arch);
			}
		} else if (config.type === "python") {
			config.env["MIRRORD_DETECT_DEBUGGER_PORT"] = "debugpy";
		}

		// Add a fixed range of ports that VS Code uses for debugging.
		// TODO: find a way to use MIRRORD_DETECT_DEBUGGER_PORT for other debuggers.
		config.env["MIRRORD_IGNORE_DEBUGGER_PORTS"] = "45000-65535";

		let executableFieldName = getExecutableFieldName(config);

		let executionInfo;
		if (executableFieldName in config) {
			executionInfo = await mirrordApi.binaryExecute(target, configPath, config[executableFieldName]);
		} else {
			// Even `program` is not in config, so no idea what's the executable in the end.
			executionInfo = await mirrordApi.binaryExecute(target, configPath, null);
		}

		// For sidestepping SIP on macOS. If we didn't patch, we don't change that config value.
		let patchedPath = executionInfo?.patchedPath;
		if (patchedPath) {
			config[executableFieldName] = patchedPath;
		}

		let env = executionInfo?.env;
		config.env = Object.assign({}, config.env, env);

		config.env["__MIRRORD_EXT_INJECTED"] = 'true';

		return config;
	}
}