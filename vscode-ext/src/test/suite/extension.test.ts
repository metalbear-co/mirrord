import * as assert from 'assert';
import { after } from 'mocha';
import { config } from 'process';
import { del } from 'request';

const k8s = require('@kubernetes/client-node');
const axios = require('axios');
import * as vscode from 'vscode';

suite('Extension Test Suite', () => {
    //   after(() => {
    //     vscode.window.showInformationMessage('All tests done!');
    //   });    
});

const CONFIG = {
    "accept_invalid_certificates": false,
    "target": {
        "path": "pod/test-service-abcdefg-abcd",
        "namespace": "default"
    },
    "agent": {
        "log_level": "info",
        "namespace": "default",
        "image": "",
        "image_pull_policy": "",
        "ttl": 60,
        "ephemeral": false
    },
    "feature": {
        "env": true,
        "fs": "write",
        "network": {
            "dns": false,
            "incoming": "mirror",
            "outgoing": {
                "tcp": true,
                "udp": false
            }
        }
    }
};


test('Test process receives remote traffic', async () => {
    let folders = vscode.workspace.workspaceFolders;
    if (folders === undefined) {
        throw new Error('No workspace folder found');
    }    
    let configPath = vscode.Uri.joinPath(folders[0].uri, '.mirrord/mirrord.json');

    let k8sConfig = new k8s.KubeConfig();
    k8sConfig.loadFromDefault();
    let k8sApi = k8sConfig.makeApiClient(k8s.CoreV1Api);
    let pods = await k8sApi.listNamespacedPod('default');

    let pod = pods.body.items.filter((pod: { metadata: { name: string; }; }) => pod.metadata.name.startsWith('nginx'))[0];
    let pod_name = pod.metadata.name;

    console.log("pod: ", pod_name);
    CONFIG.target.path = `pod/${pod_name}`;

    // create .mirrord directory
    await vscode.workspace.fs.createDirectory(vscode.Uri.joinPath(folders[0].uri, '.mirrord'));
    await vscode.workspace.fs.writeFile(configPath, Buffer.from(JSON.stringify(CONFIG, null, 2)));

    const extension = await vscode.extensions.all.filter((extension) => extension.id === 'MetalBear.mirrord')[0];
    if (extension === undefined) {
        throw new Error('Extension not found');
    }
    
    await vscode.commands.executeCommand('mirrord.toggleMirroring');

    // set a breakpoint on line 30 of app_flask.py
    let breakpoint = new vscode.SourceBreakpoint(new vscode.Location(vscode.Uri.joinPath(folders[0].uri, 'app_flask.py'), new vscode.Position(29, 0)));
    await vscode.debug.addBreakpoints([
        breakpoint,
    ]);

    // start debugging
    await vscode.debug.startDebugging(folders[0], {
        "name": 'Python: Current File',
        "type": 'python',
        "request": 'launch',
        "program": "${workspaceFolder}/app_flask.py",                               
        "console": "integratedTerminal",
        "cwd": "${workspaceFolder}",        
    });

    // wait for breakpoint to be hit
    await vscode.debug.onDidReceiveDebugSessionCustomEvent((event) => {        
        assert.strictEqual(event.event, 'breakpoint');
    });    
    
    let session = vscode.debug.activeDebugSession;
    if (session === undefined) {
        throw new Error('No active debug session found');
    }    
    

    // // send traffic to the pod using axios
    // let ingress_address = "20.81.111.66";
    // let response = await axios.get(`http://${ingress_address}/`);
    // console.log("response: ", response.data);

    // while (true) {
    //     console.log("waiting for breakpoint to be hit");
    // }
});