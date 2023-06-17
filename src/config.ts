import * as glob from 'glob';
import * as vscode from 'vscode';
import YAML from 'yaml';
import TOML from 'toml';

// Default mirrord configuration written to .mirrord/mirrord.json on 
// clicking the gear icon
export const DEFAULT_CONFIG = `
{
    "accept_invalid_certificates": false,
    "feature": {
        "network": {
            "incoming": "mirror",
            "outgoing": true
        },
        "fs": "read",
        "env": true
    }
}
`;

// Populate the path for the project's .mirrord folder
const MIRRORD_DIR = function () {
    let folders = vscode.workspace.workspaceFolders;
    if (folders === undefined) {
        throw new Error('No workspace folder found');
    }
    return vscode.Uri.joinPath(folders[0].uri, '.mirrord');
}();

// Get the file path to the user's mirrord-config file, if it exists. 
export async function configFilePath() {
    let fileUri = vscode.Uri.joinPath(MIRRORD_DIR, '?(*.)mirrord.+(toml|json|y?(a)ml)');
    let files = glob.sync(fileUri.fsPath);
    return files[0] || '';
}

export async function openConfig() {
    let path = await configFilePath();
    if (!path) {
        const uriPath = vscode.Uri.joinPath(MIRRORD_DIR, 'mirrord.json');
        await vscode.workspace.fs.writeFile(uriPath, Buffer.from(DEFAULT_CONFIG));
        path = uriPath.fsPath;
    }
    vscode.workspace.openTextDocument(path).then(doc => {
        vscode.window.showTextDocument(doc);
    });
}

async function parseConfigFile() {
    let filePath: string = await configFilePath();
    if (filePath) {
        const file = (await vscode.workspace.fs.readFile(vscode.Uri.parse(filePath))).toString();
        if (filePath.endsWith('json')) {
            return JSON.parse(file);
        } else if (filePath.endsWith('yaml') || filePath.endsWith('yml')) {
            return YAML.parse(file);
        } else if (filePath.endsWith('toml')) {
            return TOML.parse(file);
        }
    }
}

// Gets target from config file
export async function isTargetInFile() {
    let parsed = await parseConfigFile();
    return (parsed && (typeof (parsed['target']) === 'string' || parsed['target']?.['path']));
}

// Gets namespace from config file
export async function parseNamespace() {
    let parsed = await parseConfigFile();
    return parsed?.['target']?.['namespace'];
}