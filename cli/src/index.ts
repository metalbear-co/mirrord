#!/usr/bin/env npx ts-node
const { program } = require('commander');
import { PortSpec, MirrorD, MirrorEvent, K8SAPI } from '@metalbear/mirrord-core';

function sleep(ms: number) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

interface Arguments {
    podName: string;
    ports: PortSpec[];
    namespace: string
}


let mirror: MirrorD | null = null;
let run = true;


function parseArgs(): Arguments {
    program
        .name("mirrord")
        .description('Mirror traffic from specified pod to localhost')
        .argument('<podName>', 'Name of pod to mirror')
        .option('-p, --port [localport:remoteport...]', 'Local port to send to and remote port to capture from', ['8000:80'])
        .option('-n, --namespace [namespace]', 'Namespace to use', 'default')

    let args = program.parse(process.argv);
    let options = args.opts();
    let ports = options.port.map((p: String) => {
        let [localPort, remotePort] = p.split(':');
        return {
            localPort: parseInt(localPort),
            remotePort: parseInt(remotePort)
        };
    });
    return {
        podName: args.args[0],
        ports: ports,
        namespace: options.namespace
    };
}

function updateCallback(event: MirrorEvent, data: any) {
    switch (event) {
        case MirrorEvent.NewConnection:
            console.log('New connection to port ' + data.toString());
            break
        case MirrorEvent.PacketReceived:
            console.log('Packet received');
            break
        case MirrorEvent.ConnectionClosed:
            console.log('Connection closed');
            break
    }
}

async function exitHandler() {
    if (mirror) {
        await mirror.stop();
        mirror = null;
    }
    run = false;
}

['SIGHUP', 'SIGINT', 'SIGQUIT', 'SIGILL', 'SIGTRAP', 'SIGABRT',
    'SIGBUS', 'SIGFPE', 'SIGUSR1', 'SIGSEGV', 'SIGUSR2', 'SIGTERM'
].forEach(function (sig) {
    process.on(sig, function () {
        exitHandler();
    });
});

async function main() {
    const args = parseArgs();
    let api = new K8SAPI();
    let podData: any;
    try {
        podData = await api.getPodData(args.podName, args.namespace);
    }
    catch (e: any) {
        if (e.body) {
            console.log(e.body.message);
        } else {
            console.log(e);
        }
        return;
    }
    mirror = new MirrorD(podData.nodeName, podData.containerID, args.ports, args.namespace, api, updateCallback);
    await mirror.start();
    console.log("To terminate, press Ctrl+C");
    while (run) {
        await sleep(1000);
    }

}

main();