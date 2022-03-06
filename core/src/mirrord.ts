const k8s = require('@kubernetes/client-node');
const shortid = require('short-uuid');
const net = require('net');
const stream = require('stream');

export interface PortSpec {
    localPort: number,
    remotePort: number,
}


export interface PodData {
    nodeName: string;
    containerID: string;
}

function sleep(ms: number) {
    return new Promise(resolve => setTimeout(resolve, ms));
}


function createMirrordPodDefinition(agentPodName: String, nodeName: String, containerID: String, ports: number[]): any {
    let portArguments = ports.map((p: number) => {
        return ['--ports', p.toString()]
    }).flat();

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

export class K8SAPI {
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

    async listPods(namespace: string): Promise<any> {
        return await this.api.listNamespacedPod(namespace);
    }
    
    async getPodData(podName: String, namespace: String): Promise<PodData> {
        const rawData = (await this.api.readNamespacedPod(podName, namespace)).body;
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
    async waitForPodReady(podName: String, namespace: String): Promise<boolean> {
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

export enum MirrorEvent {
    NewConnection,
    PacketReceived,
    ConnectionClosed
}

export class Tunnel {
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
        data.toString().split('\n').forEach(this.handleMessage.bind(this));
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

export class MirrorD {
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
        this.updateCallback = updateCallback;
    }


    async start() {
        await this.k8sApi.createAgentPod(this.agentPodName, this.nodeName, this.containerID, this.namespace, this.ports.map(p => p.remotePort));
        const ready = await this.k8sApi.waitForPodReady(this.agentPodName, this.namespace);
        if (!ready) {
            console.error("Pod isn't ready after timeout");
            return;
        }
        await this.tunnelTraffic();
    }

    async tunnelTraffic() {
        let log = this.k8sApi.getLog();
        let logStream = new stream.PassThrough();
        this.tunnel = new Tunnel(Object.assign({}, ...this.ports.map(p => ({ [p.remotePort]: p.localPort }))), this.updateCallback);
        logStream.on('data', this.tunnel.newDataCallback.bind(this.tunnel));
        this.logRequest = await log.log(this.namespace, this.agentPodName, '', logStream, (err: any) => {
            console.log(err);
        }, { follow: true, tailLines: 0, pretty: false, timestamps: false });
    }

    async stop() {
        if (this.logRequest) {
            try {
                await this.logRequest.end()
            }
            catch (err) {
                console.error(err);
            }

        }
        if (this.tunnel) {
            try {
                await this.tunnel.close();
            } catch (err) {
                console.error(err)
            }
        }
        await this.k8sApi.deletePod(this.agentPodName, this.namespace);
    }
}