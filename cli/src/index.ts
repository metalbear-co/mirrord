#!/usr/bin/env npx ts-node

import { Socket } from "net";

const { program } = require('commander');
const k8s = require('@kubernetes/client-node');
const shortid = require('short-uuid');
const net = require('net');
const stream = require('stream');

interface PortSpec {
    localPort: number,
    remotePort: number,
}

interface Arguments {
    podName: string;
    ports: PortSpec[];
    namespace: string
}

function sleep(ms: number) {
    return new Promise(resolve => setTimeout(resolve, ms));
}

function parseArgs(): Arguments {
    program
        .name("mirrord")
        .description('Mirror traffic from specified pod to localhost')
        .argument('<podName>', 'Name of pod to mirror')
        .option('-p, --port [localport:remoteport...]', 'Local port to send to and remote port to capture from', ['8080:80'])
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

interface PodData {
    nodeName: string;
    containerID: string;
}

function createMirrordPodDefinition(agentPodName: String, nodeName: String, containerID: String, ports: number[]): any {
    let portArguments = ports.map((p: number) => {
        return ['--ports', p.toString()]
    });

    return {
        metadata: { name: agentPodName },
        spec: {
            hostPID: true,
            nodeName: nodeName,
            restartPolicy: 'Never',
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
                        containerID,
                        ...portArguments
                    ]
                }
            ]
        }

    };
}

class K8SAPI {
    api: any;
    config: any;

    constructor() {
        const kc = new k8s.KubeConfig();
        kc.loadFromDefault();
        this.config = kc;
        this.api = kc.makeApiClient(k8s.CoreV1Api);
    }

    getLog() {
        return new k8s.Log(this.config);
    }

    async getPodData(podName: String, namespace: String): Promise<PodData> {
        const rawData = await this.api.readNamespacedPod(podName, namespace);
        return {
            nodeName: rawData.spec.nodeName,
            containerID: rawData.status.containerStatuses[0].containerID.split('//')[1]
        }
    }

    async createAgentPod(podname: String, nodeName: String, containerID: String, namespace: String, ports: number[]): Promise<void> {
        const agentPod = createMirrordPodDefinition(podname, nodeName, containerID, ports);
        await this.api.createNamespacedPod(namespace, agentPod);
    }

    /*
    Waits for a specific pod to be running. Returns false if pod is not running after timeout.
    */
    async waitForPodReady(podName: String, namespace: String, packetReceivedCallback?: () => void): Promise<boolean> {
        let attempts = 0;
        while (attempts < 100) { //TODO: Must be a good way to do this
            let status = await this.api.readNamespacedPodStatus(podName, namespace);
            if (status.body.status.phase === "Running") {
                return true;
            }
            attempts += 1;
            await sleep(100);
        }
        return false;
    }

    async deletePod(podName: string, namespace: String) {
        await this.api.deleteNamespacedPod(podName, namespace);
    }
}

enum MirrorEvent {
    NewConnection,
    PacketReceived,
    ConnectionClosed
}

class Tunnel {
    // Figure how to get type of net.Socket
    connections: Map<number, any>;
    portsMap: { [port: number]: number }
    eventCallback?: (event: MirrorEvent, data?: any) => void;

    constructor(portsMap: { [port: number]: number },
        eventCallback?: (event: MirrorEvent, data: any) => void) {
        this.connections = new Map();
        this.portsMap = portsMap;
        this.eventCallback = eventCallback;
    }

    close() {
        for (let value of this.connections.values()) {
            value.end()
        }
        this.connections = new Map();
    }
    async newDataCallback(data: Buffer) {
        data.toString().split('\n').forEach(this.handleMessage);
    }

    updateEvent(event: MirrorEvent, data?: any) {
        if (this.eventCallback) {
            this.eventCallback(event, data);
        }
    }

    async handleMessage(rawJson: string) {
        if (!rawJson) {
            return
        }
        let parsed = JSON.parse(rawJson);
        if (parsed['type'] === 'Error') {
            console.error('Agent error' + parsed.content.msg);
            return
        }
        else if (parsed['type'] === 'Connected') {
            let socket = new net.Socket();
            socket.connect(this.portsMap[parsed.content.port], '127.0.0.1');
            this.connections.set(parsed.content.connectionID, socket)
            this.updateEvent(MirrorEvent.NewConnection, parsed.content.port);
        }
        else if (parsed['type'] === 'Data') {
            let socket = this.connections.get(parsed.content.connectionID);
            socket.write(Buffer.from(parsed.content.data, 'base64'));
            this.updateEvent(MirrorEvent.PacketReceived);
        }
        else if (parsed['type'] === 'TCPEnded') {
            let socket = this.connections.get(parsed.content.connectionID);
            socket.end();
            this.connections.delete(parsed.content.connectionID);
            this.updateEvent(MirrorEvent.ConnectionClosed);
        }
    }
}

class MirrorD {
    k8sApi: K8SAPI;
    nodeName: string;
    containerID: string;
    namespace: string;
    ports: PortSpec[];
    agentPodName: string;
    tunnel?: Tunnel;
    logRequest?: any;
    updateCallback?: (event: MirrorEvent, data: any) => void;


    constructor(nodeName: string, containerID: string, ports: PortSpec[], namespace: string,
        k8sApi?: K8SAPI,
        updateCallback?: (event: MirrorEvent, data: any) => void) {
        this.k8sApi = k8sApi || new K8SAPI();
        this.nodeName = nodeName;
        this.containerID = containerID;
        this.ports = ports;
        this.namespace = namespace;
        this.agentPodName = 'mirrord-' + shortid.generate().toLowerCase();
    }


    async start() {
        await this.k8sApi.createAgentPod(this.agentPodName, this.nodeName, this.containerID, this.namespace, this.ports.map(p => p.remotePort));
        const ready = await this.k8sApi.waitForPodReady(this.agentPodName, this.namespace);
        if (!ready) {
            console.error("Pod isn't ready after timeout");
            return;
        }
    }

    async tunnelTraffic() {
        let log = this.k8sApi.getLog();
        let logStream = new stream.PassThrough();
        let tunnel = new Tunnel(Object.assign({}, ...this.ports.map(p => ({[p.remotePort]: p.localPort}))), this.updateCallback);
        logStream.on('data', tunnel.newDataCallback);
        this.logRequest = await log.log(this.namespace, this.agentPodName, '', logStream, (err: any) => {
            console.log(err);
        }, { follow: true, tailLines: 0, pretty: false, timestamps: false });
    }

    async stop() {
        if (this.logRequest) {
            this.logRequest.abort();
        }
        if (this.tunnel) {
            this.tunnel.close();
        }
        await this.k8sApi.deletePod(this.agentPodName, this.namespace);
    }
}

let tunnel: Tunnel | null = null;

function updateCallback(event: MirrorEvent, data: any) {
    console.log(event, data);
}

async function main() {
    const args = parseArgs();
    let api = new K8SAPI();
    const podData = await api.getPodData(args.podName, args.namespace);
    const mirrord = new MirrorD(podData.nodeName, podData.containerID, args.ports, args.namespace, api, updateCallback);
    await mirrord.start();

}

main();