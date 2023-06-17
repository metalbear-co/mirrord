import axios from 'axios';
import which from 'which';
import { globalContext } from './extension';
import { MirrordAPI } from './api';
import { Utils } from 'vscode-uri'
import * as fs from 'node:fs';
import { Uri, workspace } from 'vscode';

const mirrordBinaryEndpoint = 'https://version.mirrord.dev/v1/version';
const binaryCheckInterval = 1000 * 60 * 3;


/**
 * Returns path to mirrord binary
 */
export async function getMirrordBinaryPath(): Promise<string | undefined> {
    const latestVersion = await getLatestSupportedVersion();
    const foundMirrord = await which("mirrord");
    if (foundMirrord) {
        const api = new MirrordAPI(foundMirrord);
        const installedVersion = await api.getBinaryVersion();
        if (installedVersion === latestVersion) {
            return foundMirrord;
        }
    }
    const extensionMirrordPath = Utils.joinPath(globalContext.extensionUri, 'mirrord');
    
    let binaryExists = false;
    try {
        await workspace.fs.stat(extensionMirrordPath)
        binaryExists = true;
    } catch (e) {
        // that's okay
    }

    if (binaryExists) {
        const api = new MirrordAPI(extensionMirrordPath.fsPath);
        const installedVersion = await api.getBinaryVersion();
        if (installedVersion === latestVersion) {
            return extensionMirrordPath.fsPath;
        }
    }
    
    
}

/**
 * 
 * @returns The latest supported version of mirrord for current extension version
 */
async function getLatestSupportedVersion(): Promise<string> {
    let lastChecked = globalContext.globalState.get('binaryLastChecked', 0);
    let lastBinaryVersion = globalContext.globalState.get('lastBinaryVersion', '');

    if (lastBinaryVersion && lastChecked > Date.now() - binaryCheckInterval) {
        return lastBinaryVersion;
    }

    const response = await axios.get(mirrordBinaryEndpoint, {
        "params": { "source": 1, "version": globalContext.extension.packageJSON.version }
    });

    globalContext.globalState.update('binaryLastChecked', Date.now());
    globalContext.globalState.update('lastBinaryVersion', response.data);
    return response.data as string;
}

function getMirrordDownloadUrl(version: string): string {
    
}

/**
 * 
 * @param dest_path Path to download the binary to
 */
async function downloadMirrordBinary(dest_path: Uri, version: string): Promise<void> {
    fs.chmodSync(dest_path.fsPath, 0o755);
    const response = await axios.get(
        url);
    https://github.com/metalbear-co/mirrord/releases/download/$version/mirrord_$OS\_$ARCH
    workspace.fs.writeFile();
}